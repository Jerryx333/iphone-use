#import "SpikeBridge.h"
#import <objc/message.h>
#import <objc/runtime.h>
#import <dlfcn.h>
#import <unistd.h>
#import <string.h>
#import <math.h>

static id daemonSession;
static NSUInteger generation;
static BOOL sharedSessionAttempted; // Process lifetime; never reset by Stop.
static BOOL sharedSessionInvalidated;
static id invalidationToken;
static id Object(id target, NSString *name) {
    SEL sel = NSSelectorFromString(name);
    if (![target respondsToSelector:sel]) return nil;
    return ((id (*)(id, SEL))objc_msgSend)(target, sel);
}
static void Require(id target, NSString *name) {
    if (![target respondsToSelector:NSSelectorFromString(name)])
        @throw [NSException exceptionWithName:@"MissingPrivateAPI" reason:name userInfo:nil];
}
static id Value(id target, NSString *key) {
    @try { return [target valueForKey:key]; } @catch (__unused NSException *e) { return nil; }
}
static NSString *Exception(NSException *e) { return [NSString stringWithFormat:@"%@: %@", e.name, e.reason]; }
// Compare ABI components, not offsets embedded in the full type encoding.
static NSMethodSignature *CheckedSignature(id target, NSString *name, const char *result,
                                          NSArray<NSString *> *arguments, void (^log)(NSString *)) {
    Require(target, name);
    NSMethodSignature *signature = [target methodSignatureForSelector:NSSelectorFromString(name)];
    log([NSString stringWithFormat:@"ABI %@: return=%s arguments=%lu", name,
         signature ? signature.methodReturnType : "missing", (unsigned long)signature.numberOfArguments]);
    if (!signature || strcmp(signature.methodReturnType, result) != 0 ||
        signature.numberOfArguments != arguments.count + 2)
        @throw [NSException exceptionWithName:@"UnsupportedPrivateABI" reason:name userInfo:nil];
    for (NSUInteger index = 0; index < arguments.count; index++) {
        const char *type = [signature getArgumentTypeAtIndex:index + 2];
        log([NSString stringWithFormat:@"ABI %@ argument %lu: %s", name, (unsigned long)index, type]);
        if (strcmp(type, arguments[index].UTF8String) != 0)
            @throw [NSException exceptionWithName:@"UnsupportedPrivateABI" reason:name userInfo:nil];
    }
    return signature;
}
static id CheckedObject(id target, NSString *name, void (^log)(NSString *)) {
    CheckedSignature(target, name, @encode(id), @[], log);
    return ((id (*)(id, SEL))objc_msgSend)(target, NSSelectorFromString(name));
}
static BOOL CheckedBool(id target, NSString *name, void (^log)(NSString *)) {
    CheckedSignature(target, name, @encode(BOOL), @[], log);
    return ((BOOL (*)(id, SEL))objc_msgSend)(target, NSSelectorFromString(name));
}
// Log synchronously before entering private code, so the UI watchdog can identify a hang.
#define STEP(prefix, name, ...) ({ \
    CFAbsoluteTime stepStart = CFAbsoluteTimeGetCurrent(); \
    log([NSString stringWithFormat:@"%@ step %@ start", prefix, name]); \
    __typeof__((__VA_ARGS__)) stepResult = (__VA_ARGS__); \
    log([NSString stringWithFormat:@"%@ step %@ done %.0f ms", prefix, name, (CFAbsoluteTimeGetCurrent()-stepStart)*1000]); \
    stepResult; \
})
static void VoidStep(void (^log)(NSString *), NSString *name, void (^call)(void)) {
    CFAbsoluteTime start = CFAbsoluteTimeGetCurrent();
    log([NSString stringWithFormat:@"tap step %@ start", name]);
    call();
    log([NSString stringWithFormat:@"tap step %@ done %.0f ms", name, (CFAbsoluteTimeGetCurrent()-start)*1000]);
}
@implementation SpikeBridge

// Copied from runner/IPhoneUseRunner/IPhoneUseRunnerUITests/IPURBridge.m,
// quiescence bypass (see that file/runner README for MIT attribution).
static void SpikeNoopVoidMethod(Class cls, NSString *selectorName, NSMutableArray<NSString *> *patched)
{
  if (cls == Nil) return;
  Method method = class_getInstanceMethod(cls, NSSelectorFromString(selectorName));
  if (method == NULL) return;
  char returnType[8] = {0};
  method_getReturnType(method, returnType, sizeof(returnType));
  // Only void waits are neutralised; a wait that returns a verdict keeps its implementation.
  if (returnType[0] != 'v') return;
  // Extra arguments (BOOL flags, an activity) are ignored by the block; that is safe for the
  // register-passed scalars and pointers these selectors take.
  IMP noop = imp_implementationWithBlock(^(__unused id receiver) {
  });
  method_setImplementation(method, noop);
  [patched addObject:[NSString stringWithFormat:@"-[%@ %@]", NSStringFromClass(cls), selectorName]];
}

// Independently implemented from the idle-notification behavior described in
// willfaust/TouchSynthesis public README/comments; no source code copied.
static void SpikeNotifyIdleImmediately(Class cls, NSString *selectorName, NSMutableArray<NSString *> *patched)
{
  if (cls == Nil) return;
  SEL selector = NSSelectorFromString(selectorName);
  Method method = class_getInstanceMethod(cls, selector);
  if (method == NULL) return;
  char returnType[8] = {0};
  char callbackType[16] = {0};
  method_getReturnType(method, returnType, sizeof(returnType));
  if (method_getNumberOfArguments(method) != 3 || returnType[0] != 'v') return;
  method_getArgumentType(method, 2, callbackType, sizeof(callbackType));
  if (strcmp(callbackType, "@?") != 0) return;
  IMP immediate = imp_implementationWithBlock(^(__unused id receiver, void (^reply)(BOOL)) {
    if (reply) reply(YES);
  });
  // Add an override for inherited methods rather than mutating the superclass.
  if (!class_addMethod(cls, selector, immediate, method_getTypeEncoding(method)))
    method_setImplementation(class_getInstanceMethod(cls, selector), immediate);
  [patched addObject:[NSString stringWithFormat:@"-[%@ %@]", NSStringFromClass(cls), selectorName]];
}

+ (NSArray<NSString *> *)installQuiescenceBypass
{
  static NSArray<NSString *> *installed;
  static dispatch_once_t once;
  dispatch_once(&once, ^{
    NSMutableArray<NSString *> *patched = [NSMutableArray array];
    Class process = NSClassFromString(@"XCUIApplicationProcess");
    SpikeNoopVoidMethod(process, @"waitForQuiescenceIncludingAnimationsIdle:", patched);
    SpikeNoopVoidMethod(process, @"waitForQuiescenceIncludingAnimationsIdle:isPreEvent:", patched);
    SpikeNoopVoidMethod(process, @"waitForQuiescenceIncludingAnimationsIdle:usingActivity:isPreEvent:", patched);
    Class axClient = NSClassFromString(@"XCAXClient_iOS");
    SpikeNoopVoidMethod(axClient, @"waitForQuiescenceOnAllForegroundApplicationsAsPreEvent:", patched);
    Class application = NSClassFromString(@"XCUIApplication");
    SpikeNoopVoidMethod(application, @"_waitForQuiescence", patched);
    SpikeNoopVoidMethod(application, @"_waitForQuiescenceAsPreEvent:", patched);
    for (NSString *className in @[@"XCTRunnerAutomationSession", @"XCTAutomationSession"]) {
      Class sessionClass = NSClassFromString(className);
      for (NSString *selectorName in @[@"notifyWhenMainRunLoopIsIdle:", @"notifyWhenAnimationsAreIdle:",
                                      @"_XCT_notifyWhenMainRunLoopIsIdle:", @"_XCT_notifyWhenAnimationsAreIdle:"]) {
        SpikeNotifyIdleImmediately(sessionClass, selectorName, patched);
      }
    }
    installed = patched.copy;
  });
  return installed;
}

+ (void)loadXCTestWithLog:(void (^)(NSString *))log completion:(void (^)(NSString *))completion {
    // All generation/session mutations occur on main. dlopen handles intentionally remain loaded.
    NSAssert(NSThread.isMainThread, @"bootstrap on main");
    NSUInteger token = ++generation;
    NSArray *frameworks = @[@"PrivateFrameworks/XCTestSupport", @"PrivateFrameworks/XCTTargetBootstrap",
                            @"PrivateFrameworks/XCTestCore", @"Frameworks/XCUIAutomation", @"Frameworks/XCTest"];
    BOOL loaded = NO;
    for (NSString *relative in frameworks) {
        loaded = NO;
        NSString *name = relative.lastPathComponent;
        for (NSString *root in @[@"/System/Developer/Library", @"/Developer/Library"]) {
            NSString *path = [NSString stringWithFormat:@"%@/%@.framework/%@", root, relative, name];
            dlerror();
            void *handle = dlopen(path.UTF8String, RTLD_NOW | RTLD_GLOBAL);
            const char *error = dlerror();
            log([NSString stringWithFormat:@"dlopen %@: %@", path, handle ? @"OK" : (error ? @(error) : @"unknown error")]);
            if (handle) { loaded = YES; break; }
        }
    }
    if (!loaded) { completion(@"XCTest unavailable: mount the personalized DDI from Xcode first."); return; }
    Class cls = NSClassFromString(@"XCTRunnerDaemonSession");
    NSString *selector = @"initiateSharedSessionWithCompletion:";
    if (![cls respondsToSelector:NSSelectorFromString(selector)]) { completion(@"XCTRunnerDaemonSession initiation API missing"); return; }
    @try {
        if ([self needsRelaunch]) {
            completion(@"needs_relaunch: shared daemon session invalidated; terminate and relaunch the process"); return;
        }
        id existing;
        @synchronized(self) { existing = daemonSession; }
        // This getter does not implicitly initiate; sharedSession does.
        if (!existing && !sharedSessionAttempted && [cls respondsToSelector:NSSelectorFromString(@"sharedSessionIfInitiated")])
            existing = CheckedObject(cls, @"sharedSessionIfInitiated", log);
        if (existing) {
            @synchronized(self) { daemonSession = existing; }
            [self observeSessionInvalidation:existing log:log];
            if ([self needsRelaunch]) { completion(@"needs_relaunch: shared session cannot be safely reused"); return; }
            log(@"reusing existing shared daemon session; initiate skipped");
            completion(nil); return;
        }
        if (sharedSessionAttempted) {
            completion(@"needs_relaunch: shared session initiation already attempted without a reusable session; terminate and relaunch the process"); return;
        }
        CheckedSignature(cls, selector, @encode(void), @[@"@?"], log);
    } @catch (NSException *e) {
        completion([@"needs_relaunch: cannot reuse shared daemon session: " stringByAppendingString:Exception(e)]); return;
    }
    __block BOOL finished = NO;
    CFAbsoluteTime started = CFAbsoluteTimeGetCurrent();
    void (^finish)(id, NSError *) = ^(id session, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            // Even a late callback belongs to the process-wide singleton. Keep it for reuse.
            @synchronized(SpikeBridge.class) { daemonSession = session; }
            if (session) [self observeSessionInvalidation:session log:log];
            if (finished || token != generation) return;
            finished = YES;
            if ([self needsRelaunch]) { completion(@"needs_relaunch: shared session cannot be safely reused"); return; }
            log([NSString stringWithFormat:@"daemon session %.0f ms: %@", (CFAbsoluteTimeGetCurrent()-started)*1000, error ?: (session ? @"ready" : @"nil")]);
            completion(session ? nil : [@"needs_relaunch: " stringByAppendingString:(error.localizedDescription ?: @"daemon session returned nil")]);
        });
    };
    @try {
        sharedSessionAttempted = YES; // Set before private code can call back or throw.
        ((void (*)(id, SEL, id))objc_msgSend)(cls, NSSelectorFromString(selector), finish);
    } @catch (NSException *e) { finished = YES; completion([@"needs_relaunch: " stringByAppendingString:Exception(e)]); return; }
    for (int seconds = 10; seconds <= 90; seconds += 10) {
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)seconds*NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            if (finished || token != generation) return;
            log([NSString stringWithFormat:@"waiting for daemon session: %d / 90 s", seconds]);
            if (seconds == 90) { finished = YES; completion(@"needs_relaunch: daemon session timeout (90 s); terminate and relaunch the process"); }
        });
    }
}
+ (void)observeSessionInvalidation:(id)session log:(void (^)(NSString *))log {
    if (invalidationToken) return;
    @try {
        CheckedSignature(session, @"registerInvalidationHandler:", @encode(id), @[@"@?"], log);
        void (^handler)(void) = ^{
            @synchronized(self) { sharedSessionInvalidated = YES; }
            log(@"needs_relaunch: shared daemon session invalidated; terminate and relaunch the process");
        };
        invalidationToken = ((id (*)(id, SEL, id))objc_msgSend)(session, NSSelectorFromString(@"registerInvalidationHandler:"), handler);
    } @catch (NSException *e) {
        @synchronized(self) { sharedSessionInvalidated = YES; }
        log([@"needs_relaunch: session invalidation observer unavailable: " stringByAppendingString:Exception(e)]);
    }
}
+ (BOOL)needsRelaunch {
    @synchronized(self) { return sharedSessionInvalidated; }
}
+ (NSNumber *)automationModeEnabledWithLog:(void (^)(NSString *))log {
    // XCTAutomationSupport's arm64 acceptNewConnection: calls this with no arguments
    // and tests bit 0 of w0. Availability and access from this app are runtime-dependent.
    void *handle = dlopen("/System/Library/PrivateFrameworks/AutomationMode.framework/AutomationMode", RTLD_NOW | RTLD_LOCAL);
    BOOL (*query)(void) = handle ? (BOOL (*)(void))dlsym(handle, "XAMIsAutomationModeEnabled") : NULL;
    if (!query) { log(@"automation mode status unavailable; using enable"); return nil; }
    @try {
        NSNumber *enabled = @(query());
        log([NSString stringWithFormat:@"automation mode status (XAMIsAutomationModeEnabled): %@", enabled]);
        return enabled; // Keep the framework loaded, like the XCTest handles.
    } @catch (NSException *e) {
        log([@"automation mode status unavailable; using enable: " stringByAppendingString:Exception(e)]);
        return nil;
    }
}
+ (NSString *)enableAutomation {
    // Scope the framework timeout to this call, restoring it before any other action.
    double (*getTimeout)(void) = (double (*)(void))dlsym(RTLD_DEFAULT, "_XCTApplicationStateTimeout");
    void (*setTimeout)(double) = (void (*)(double))dlsym(RTLD_DEFAULT, "_XCTSetApplicationStateTimeout");
    double previous = 0;
    BOOL changed = NO;
    @try {
        id device;
        @synchronized(SpikeBridge.class) { device = daemonSession; }
        CheckedSignature(device, @"enableAutomationModeWithError:", @encode(BOOL), @[@"^@"], ^(__unused NSString *message) {});
        if (getTimeout && setTimeout) { previous = getTimeout(); setTimeout(20); changed = YES; }
        NSError *error = nil;
        BOOL ok = ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(device, NSSelectorFromString(@"enableAutomationModeWithError:"), &error);
        return ok ? nil : (error.localizedDescription ?: @"enableAutomationMode returned NO");
    } @catch (NSException *e) { return Exception(e); }
    @finally { if (changed) setTimeout(previous); }
}
+ (NSDictionary *)inspectUIAutomationWithLog:(void (^)(NSString *))log {
    NSMutableDictionary *result = [NSMutableDictionary dictionary];
    @try {
        Class cls = NSClassFromString(@"XCTestConfiguration");
        id configuration = CheckedObject(cls, @"activeTestConfiguration", log);
        result[@"configurationPresent"] = @(configuration != nil);
        if (configuration) result[@"initializeForUITesting"] = @(CheckedBool(configuration, @"initializeForUITesting", log));
        id device = CheckedObject(NSClassFromString(@"XCUIDevice"), @"sharedDevice", log);
        result[@"configuredForUITesting"] = @(CheckedBool(device, @"configuredForUITesting", log));
        id client = CheckedObject(device, @"accessibilityInterface", log);
        result[@"clientClass"] = client ? NSStringFromClass([client class]) : @"nil";
        result[@"mainThread"] = @(NSThread.isMainThread);
    } @catch (NSException *e) { result[@"error"] = Exception(e); }
    return result;
}
+ (NSDictionary *)loadAccessibilityWithLog:(void (^)(NSString *))log {
    NSMutableDictionary *result = [@{@"experiment": @"E1", @"success": @NO} mutableCopy];
    CFAbsoluteTime started = CFAbsoluteTimeGetCurrent();
    @try {
        if (NSThread.isMainThread)
            @throw [NSException exceptionWithName:@"ExperimentThread" reason:@"E1 must use the AX worker" userInfo:nil];
        id session;
        @synchronized(SpikeBridge.class) { session = daemonSession; }
        if (!session) @throw [NSException exceptionWithName:@"ExperimentState" reason:@"daemon session absent" userInfo:nil];
        id device = STEP(@"ax-load", @"sharedDevice", CheckedObject(NSClassFromString(@"XCUIDevice"), @"sharedDevice", log));
        id client = STEP(@"ax-load", @"accessibilityInterface", CheckedObject(device, @"accessibilityInterface", log));
        result[@"clientClass"] = client ? NSStringFromClass([client class]) : @"nil";
        CheckedSignature(client, @"loadAccessibility:", @encode(BOOL), @[@"^@"], log);
        NSError *error = nil;
        BOOL ok = STEP(@"ax-load", @"loadAccessibility:", ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(client, NSSelectorFromString(@"loadAccessibility:"), &error));
        result[@"success"] = @(ok);
        if (error) {
            result[@"error"] = error.localizedDescription;
            result[@"errorDomain"] = error.domain;
            result[@"errorCode"] = @(error.code);
        } else if (!ok) result[@"error"] = @"loadAccessibility returned NO without NSError";
    } @catch (NSException *e) { result[@"error"] = Exception(e); }
    result[@"elapsedMs"] = @((CFAbsoluteTimeGetCurrent() - started) * 1000);
    return result;
}
+ (NSDictionary *)loadAccessibilityVariant:(NSString *)variant log:(void (^)(NSString *))log {
    NSMutableArray *notifications = [NSMutableArray array];
    NSMutableDictionary *result = [@{@"experiment": variant, @"clientClass": @"nil",
                                    @"success": @NO, @"registeredNotifications": notifications} mutableCopy];
    CFAbsoluteTime started = CFAbsoluteTimeGetCurrent();
    NSString *failedStep = @"variant";
    NSString *prefix = [NSString stringWithFormat:@"ax-load-%@", variant];
    NSError *error = nil;
    @try {
        log([NSString stringWithFormat:@"%@ step variant start", prefix]);
        if (![variant isEqualToString:@"E1b"] && ![variant isEqualToString:@"E1c"])
            @throw [NSException exceptionWithName:@"ExperimentVariant" reason:@"expected E1b or E1c" userInfo:nil];
        log([NSString stringWithFormat:@"%@ step variant done", prefix]);
        failedStep = @"thread";
        log([NSString stringWithFormat:@"%@ step thread start", prefix]);
        if (NSThread.isMainThread)
            @throw [NSException exceptionWithName:@"ExperimentThread" reason:[NSString stringWithFormat:@"%@ must use the AX worker", variant] userInfo:nil];
        log([NSString stringWithFormat:@"%@ step thread done", prefix]);
        failedStep = @"daemonSession";
        id session = STEP(prefix, failedStep, ({
            id current;
            @synchronized(SpikeBridge.class) { current = daemonSession; }
            current;
        }));
        if (!session) @throw [NSException exceptionWithName:@"ExperimentState" reason:@"daemon session absent" userInfo:nil];
        failedStep = @"sharedDevice";
        id device = STEP(prefix, failedStep, CheckedObject(NSClassFromString(@"XCUIDevice"), @"sharedDevice", log));
        failedStep = @"accessibilityInterface";
        id client = STEP(prefix, failedStep, CheckedObject(device, @"accessibilityInterface", log));
        result[@"clientClass"] = client ? NSStringFromClass([client class]) : @"nil";
        do {
            failedStep = @"_loadAccessibility:";
            CheckedSignature(client, failedStep, @encode(BOOL), @[@"^@"], log);
            BOOL ok = STEP(prefix, failedStep, ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(client, NSSelectorFromString(failedStep), &error));
            if (!ok) break;

            failedStep = @"_XCTAXIPCTimeout";
            double (*timeoutFunction)(void) = STEP(prefix, @"dlsym _XCTAXIPCTimeout", (double (*)(void))dlsym(RTLD_DEFAULT, "_XCTAXIPCTimeout"));
            if (!timeoutFunction)
                @throw [NSException exceptionWithName:@"MissingPrivateAPI" reason:failedStep userInfo:nil];
            double timeout = STEP(prefix, failedStep, timeoutFunction());
            result[@"timeoutSeconds"] = @(timeout);

            failedStep = @"_setAXTimeout:error:";
            CheckedSignature(client, failedStep, @encode(BOOL), @[@"d", @"^@"], log);
            error = nil;
            ok = STEP(prefix, failedStep, ((BOOL (*)(id, SEL, double, NSError **))objc_msgSend)(client, NSSelectorFromString(failedStep), timeout, &error));
            if (!ok) break;

            if ([variant isEqualToString:@"E1c"]) {
                failedStep = @"_registerForAXNotification:error:";
                CheckedSignature(client, failedStep, @encode(BOOL), @[@"i", @"^@"], log);
                error = nil;
                ok = STEP(prefix, @"_registerForAXNotification:4002 error:", ((BOOL (*)(id, SEL, int, NSError **))objc_msgSend)(client, NSSelectorFromString(failedStep), 4002, &error));
                if (!ok) break;
                [notifications addObject:@4002];
            }
            result[@"success"] = @YES;
        } while (NO);
        if (error) {
            result[@"error"] = error.localizedDescription;
            result[@"errorDomain"] = error.domain;
            result[@"errorCode"] = @(error.code);
        } else if (![result[@"success"] boolValue]) {
            result[@"error"] = [NSString stringWithFormat:@"%@ returned NO without NSError", failedStep];
        }
    } @catch (NSException *e) { result[@"error"] = Exception(e); }
    if (![result[@"success"] boolValue]) {
        result[@"failedStep"] = failedStep;
        log([NSString stringWithFormat:@"%@ step %@ failed: %@", prefix, failedStep, result[@"error"]]);
    }
    result[@"elapsedMs"] = @((CFAbsoluteTimeGetCurrent() - started) * 1000);
    return result;
}
static NSString *SnapshotString(id value) { return value ? [value description] : @""; }
static NSDictionary *AlertSummary(id root, NSString *source) {
    NSMutableArray *buttons = [NSMutableArray new], *texts = [NSMutableArray new];
    NSMutableArray *pending = [NSMutableArray arrayWithObject:root];
    NSUInteger visited = 0;
    while (pending.count && visited < 300) {
        id node = pending.lastObject; [pending removeLastObject]; visited++;
        NSInteger type = [Value(node, @"elementType") integerValue];
        id label = Value(node, @"label");
        if ([label isKindOfClass:NSString.class] && [label length]) {
            if (type == 9 && buttons.count < 8) [buttons addObject:label];
            if ((type == 48 || type == 52) && texts.count < 8) [texts addObject:label];
        }
        id children = Value(node, @"children");
        if ([children isKindOfClass:NSArray.class]) [pending addObjectsFromArray:[children reverseObjectEnumerator].allObjects];
    }
    return @{@"type": @([Value(root, @"elementType") integerValue]), @"source": source,
             @"label": SnapshotString(Value(root, @"label")), @"identifier": SnapshotString(Value(root, @"identifier")),
             @"frame": SnapshotString(Value(root, @"frame")), @"buttons": buttons, @"texts": texts};
}
static void SaveSnapshot(NSData *lines, NSString *rootLabel, int pid, NSMutableDictionary *result) {
    @try {
        NSFileManager *manager = NSFileManager.defaultManager;
        NSError *error = nil;
        NSURL *documents = [manager URLForDirectory:NSDocumentDirectory inDomain:NSUserDomainMask
                                  appropriateForURL:nil create:YES error:&error];
        if (!documents) { result[@"snapshotFileError"] = error.localizedDescription ?: @"Documents unavailable"; return; }
        NSURL *directory = [documents URLByAppendingPathComponent:@"snapshots" isDirectory:YES];
        if (![manager createDirectoryAtURL:directory withIntermediateDirectories:YES attributes:nil error:&error]) {
            result[@"snapshotFileError"] = error.localizedDescription; return;
        }
        NSMutableString *safeLabel = [NSMutableString new];
        __block NSUInteger characters = 0;
        [rootLabel enumerateSubstringsInRange:NSMakeRange(0, rootLabel.length) options:NSStringEnumerationByComposedCharacterSequences
                                  usingBlock:^(NSString *part, NSRange range, NSRange enclosing, BOOL *stop) {
            if (characters++ == 20) { *stop = YES; return; }
            [safeLabel appendString:[part rangeOfCharacterFromSet:NSCharacterSet.alphanumericCharacterSet.invertedSet].location == NSNotFound ? part : @"_"];
        }];
        NSDateFormatter *formatter = [NSDateFormatter new];
        formatter.locale = [NSLocale localeWithLocaleIdentifier:@"en_US_POSIX"];
        formatter.calendar = [NSCalendar calendarWithIdentifier:NSCalendarIdentifierGregorian];
        formatter.timeZone = [NSTimeZone timeZoneForSecondsFromGMT:0];
        formatter.dateFormat = @"yyyyMMdd'T'HHmmss.SSS'Z'";
        NSString *filename = [NSString stringWithFormat:@"%@-%@-%d.jsonl", [formatter stringFromDate:NSDate.date], safeLabel, pid];
        if (![lines writeToURL:[directory URLByAppendingPathComponent:filename] options:NSDataWritingAtomic error:&error]) {
            result[@"snapshotFileError"] = error.localizedDescription; return;
        }
        result[@"snapshotFile"] = [@"snapshots/" stringByAppendingString:filename];
        NSArray<NSURL *> *files = [manager contentsOfDirectoryAtURL:directory includingPropertiesForKeys:@[NSURLIsRegularFileKey] options:0 error:&error];
        if (!files) { result[@"snapshotFileError"] = error.localizedDescription; return; }
        NSMutableArray<NSURL *> *snapshots = [NSMutableArray new];
        for (NSURL *file in files) {
            NSNumber *regular = nil;
            [file getResourceValue:&regular forKey:NSURLIsRegularFileKey error:nil];
            if ([file.pathExtension isEqualToString:@"jsonl"] && regular.boolValue) [snapshots addObject:file];
        }
        [snapshots sortUsingComparator:^NSComparisonResult(NSURL *a, NSURL *b) { return [a.lastPathComponent compare:b.lastPathComponent]; }];
        for (NSUInteger index = 0; index + 30 < snapshots.count; index++) {
            if (![manager removeItemAtURL:snapshots[index] error:&error]) result[@"snapshotFileError"] = error.localizedDescription;
        }
    } @catch (NSException *e) { result[@"snapshotFileError"] = Exception(e); }
}
// The runner comparison preserves IPURBridge's mapped-attribute filtering and root fallback.
+ (NSDictionary *)readScreenWithMaxDepth:(NSInteger)maxDepth runnerParameters:(BOOL)runnerParameters log:(void (^)(NSString *))log {
    return [self screenWithMaxDepth:maxDepth runnerParameters:runnerParameters remote:NO system:NO log:log];
}
+ (NSDictionary *)remoteReadWithMaxDepth:(NSInteger)maxDepth {
    return [self screenWithMaxDepth:maxDepth runnerParameters:NO remote:YES system:NO log:^(__unused NSString *message) {}];
}
+ (NSDictionary *)remoteSystemReadWithMaxDepth:(NSInteger)maxDepth {
    return [self screenWithMaxDepth:MIN(100, MAX(1, maxDepth)) runnerParameters:NO remote:YES system:YES log:^(__unused NSString *message) {}];
}
// Input-only snapshot keeps the existing read response unchanged.
+ (NSDictionary *)remoteInputReadWithMaxDepth:(NSInteger)maxDepth {
    return [self screenWithMaxDepth:maxDepth runnerParameters:NO remote:YES system:NO input:YES log:^(__unused NSString *message) {}];
}
+ (NSDictionary *)screenWithMaxDepth:(NSInteger)maxDepth runnerParameters:(BOOL)runnerParameters remote:(BOOL)remote system:(BOOL)systemTarget log:(void (^)(NSString *))log {
    return [self screenWithMaxDepth:maxDepth runnerParameters:runnerParameters remote:remote system:systemTarget input:NO log:log];
}
+ (NSDictionary *)screenWithMaxDepth:(NSInteger)maxDepth runnerParameters:(BOOL)runnerParameters remote:(BOOL)remote system:(BOOL)systemTarget input:(BOOL)input log:(void (^)(NSString *))log {
    NSString *prefix = runnerParameters ? @"ax-runner" : @"ax-default";
    CFAbsoluteTime started = CFAbsoluteTimeGetCurrent();
    NSMutableDictionary *result = [@{@"mode": prefix, @"maxDepth": @(MAX(1, maxDepth)), @"alerts": @[], @"alertCount": @0} mutableCopy];
    @try {
        id device = STEP(prefix, @"sharedDevice", Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice"));
        id client = STEP(prefix, @"accessibilityInterface", Object(device, @"accessibilityInterface"));
        if (!client && !runnerParameters)
            client = STEP(prefix, @"XCAXClient_iOS sharedClient", Object(NSClassFromString(@"XCAXClient_iOS"), @"sharedClient"));
        Require(client, @"requestSnapshotForElement:attributes:parameters:error:");
        NSArray *active = systemTarget ? @[] : STEP(prefix, @"activeApplications", Object(client, @"activeApplications"));
        id system = STEP(prefix, @"systemApplication", Object(client, @"systemApplication"));
        id element = nil;
        for (id candidate in active) { if (![candidate isEqual:system]) { element = candidate; break; } }
        element = element ?: system;
        if (!element) { result[@"error"] = @"no AX application"; return result; }
        Require(element, @"processIdentifier");
        int pid = STEP(prefix, @"processIdentifier", ((int (*)(id, SEL))objc_msgSend)(element, NSSelectorFromString(@"processIdentifier")));
        [result addEntriesFromDictionary:@{@"pid": @(pid), @"selfRead": @(pid == getpid())}];
        log([NSString stringWithFormat:@"%@ pid=%d self-read=%@ (resolving bundle)", prefix, pid, pid == getpid() ? @"YES" : @"NO"]);
        NSString *bundle = nil;
        NSString *bundleSource = @"unknown";
        for (NSString *source in @[@"applicationProcessTracker", @"applicationMonitor"]) {
            @try {
                id provider = STEP(prefix, source, Object([source isEqualToString:@"applicationProcessTracker"] ? client : device, source));
                Require(provider, @"applicationProcessWithPID:");
                id process = STEP(prefix, @"applicationProcessWithPID:", ((id (*)(id, SEL, int))objc_msgSend)(provider, NSSelectorFromString(@"applicationProcessWithPID:"), pid));
                id candidate = STEP(prefix, @"bundleID", Object(process, @"bundleID"));
                if ([candidate isKindOfClass:NSString.class] && [candidate length] && ![candidate isEqualToString:@"unknown"]) {
                    bundle = candidate; bundleSource = source;
                }
                log([NSString stringWithFormat:@"%@ bundle resolver %@: %@", prefix, source, bundle ?: @"unknown"]);
                if (bundle) break;
            } @catch (NSException *e) { log([NSString stringWithFormat:@"%@ bundle resolver %@ failed: %@", prefix, source, Exception(e)]); }
        }
        [result addEntriesFromDictionary:@{@"bundleID": bundle ?: @"unknown", @"bundleSource": bundleSource}];
        id defaults = STEP(prefix, @"defaultParameters", Object(client, @"defaultParameters"));
        static dispatch_once_t defaultsOnce;
        if ([defaults isKindOfClass:NSDictionary.class]) {
            dispatch_once(&defaultsOnce, ^{ log([NSString stringWithFormat:@"%@ defaultParameters (all keys/values): %@", prefix, defaults]); });
        } else {
            log([NSString stringWithFormat:@"%@ defaultParameters unavailable: %@", prefix, defaults]);
            if (!runnerParameters) { result[@"error"] = @"client defaultParameters is not a dictionary"; return result; }
        }
        NSMutableDictionary *parameters = [defaults isKindOfClass:NSDictionary.class] ? [defaults mutableCopy] : [NSMutableDictionary new];
        parameters[@"maxDepth"] = @(MAX(1, maxDepth));
        if (systemTarget) { parameters[@"maxChildren"] = @2000; parameters[@"maxArrayCount"] = @2000; }
        if (runnerParameters) {
            parameters[@"maxChildren"] = @2000;
            parameters[@"maxArrayCount"] = @2000;
            parameters[@"traverseFromParentsToChildren"] = @YES;
        }
        NSArray *keyPaths = runnerParameters
            ? @[@"elementType", @"identifier", @"label", @"value", @"placeholderValue", @"frame", @"enabled", @"selected", @"hasFocus", @"children"]
            : @[@"identifier", @"value", @"label", @"frame", @"enabled", @"elementType", @"placeholderValue", @"selected", @"hasFocus"];
        Class cls = NSClassFromString(@"XCElementSnapshot");
        SEL mapper = NSSelectorFromString(@"axAttributesForElementSnapshotKeyPaths:isMacOS:");
        id attrs = keyPaths;
        if ([cls respondsToSelector:mapper]) {
            id mapped = STEP(prefix, @"axAttributesForElementSnapshotKeyPaths:isMacOS:", ((id (*)(id, SEL, id, BOOL))objc_msgSend)(cls, mapper, keyPaths, NO));
            if ([mapped isKindOfClass:NSSet.class]) mapped = [mapped allObjects];
            if ([mapped isKindOfClass:NSArray.class] && [mapped count]) {
                attrs = mapped;
                if (runnerParameters) {
                    NSArray *needed = @[@"ElementType", @"Identifier", @"Label", @"Value", @"PlaceholderValue", @"Frame", @"Enabled", @"Selected", @"Focus"];
                    NSMutableArray *filtered = [NSMutableArray new];
                    for (id attribute in mapped) {
                        for (NSString *suffix in needed) {
                            if ([[attribute description] hasSuffix:suffix]) { [filtered addObject:attribute]; break; }
                        }
                    }
                    if (filtered.count) attrs = filtered.copy;
                }
            } else if (!runnerParameters) {
                result[@"error"] = @"AX attribute mapping returned no attributes"; return result;
            }
        } else if (!runnerParameters) { Require(cls, NSStringFromSelector(mapper)); }
        log([NSString stringWithFormat:@"%@ request parameters=%@ keyPaths=%@ attributes=%@", prefix, parameters, keyPaths, attrs]);
        NSError *error = nil;
        id snapshot = STEP(prefix, @"requestSnapshotForElement:attributes:parameters:error:", ((id (*)(id, SEL, id, id, id, NSError **))objc_msgSend)(client,
            NSSelectorFromString(@"requestSnapshotForElement:attributes:parameters:error:"), element, attrs, parameters.copy, &error));
        if (!snapshot) { result[@"error"] = error.localizedDescription ?: @"snapshot returned nil"; return result; }
        id root = runnerParameters
            ? (STEP(prefix, @"_rootElementSnapshot", Value(snapshot, @"_rootElementSnapshot")) ?: snapshot)
            : STEP(prefix, @"rootElementSnapshot", Object(snapshot, @"rootElementSnapshot"));
        if (!root) { result[@"error"] = @"rootElementSnapshot returned nil"; return result; }
        id rootLabel = Value(root, @"label") ?: @"";
        NSMutableArray *pending = [NSMutableArray arrayWithObject:@{@"node": root, @"depth": @0}];
        NSMutableArray *alerts = [NSMutableArray new];
        NSMutableData *lines = [NSMutableData new];
        NSMutableArray *nodes = [NSMutableArray new];
        BOOL depthTruncated = NO;
        NSString *serializationError = nil;
        NSUInteger count = 0, labeled = 0;
        CFAbsoluteTime walkStarted = CFAbsoluteTimeGetCurrent();
        log([NSString stringWithFormat:@"%@ step walkSnapshot start", prefix]);
        while (pending.count && count < 2000) {
            NSDictionary *entry = pending.lastObject; [pending removeLastObject]; count++;
            id node = entry[@"node"];
            NSUInteger depth = [entry[@"depth"] unsignedIntegerValue];
            // AX caps the server-side snapshot at maxDepth levels. At that frontier
            // absence of children cannot prove that the complete tree was returned.
            if (remote && depth >= MAX(1, maxDepth) - 1) depthTruncated = YES;
            NSInteger type = [Value(node, @"elementType") integerValue];
            NSString *identifier = SnapshotString(Value(node, @"identifier"));
            BOOL alertType = type == 5 || type == 7 || type == 8;
            BOOL dialogIdentifier = [identifier hasPrefix:@"SFDialog"] && ![identifier isEqualToString:@"SFDialogViewMessageText"];
            if ((alertType || dialogIdentifier) && alerts.count < 5) {
                [alerts addObject:AlertSummary(node, alertType ? @"elementType" : @"identifier")];
            }
            NSString *value = SnapshotString(Value(node, @"value"));
            NSDictionary *row = @{@"i": @(count), @"depth": @(depth), @"type": @(type),
                @"label": SnapshotString(Value(node, @"label")), @"identifier": identifier,
                @"value": [value substringToIndex:MIN(value.length, 200)], @"frame": SnapshotString(Value(node, @"frame"))};
            if (remote) {
                id frameValue = Value(node, @"frame");
                CGRect rect = [frameValue isKindOfClass:NSValue.class] ? [frameValue CGRectValue] : CGRectZero;
                if (!isfinite(rect.origin.x) || !isfinite(rect.origin.y) || !isfinite(rect.size.width) || !isfinite(rect.size.height)) rect = CGRectZero;
                NSMutableDictionary *remoteNode = [@{@"i": @(count - 1), @"depth": @(depth), @"type": @(type),
                    @"label": SnapshotString(Value(node, @"label")), @"identifier": identifier,
                    @"value": [value substringToIndex:MIN(value.length, 1000)],
                    @"frame": @{@"x": @(rect.origin.x), @"y": @(rect.origin.y), @"w": @(rect.size.width), @"h": @(rect.size.height)},
                    @"enabled": @([Value(node, @"enabled") boolValue])} mutableCopy];
                id focus = input ? Value(node, @"hasFocus") : nil;
                if ([focus isKindOfClass:NSNumber.class]) remoteNode[@"focused"] = @([focus boolValue]);
                [nodes addObject:remoteNode];
            }
            NSError *rowError = nil;
            NSData *data = [NSJSONSerialization dataWithJSONObject:row options:0 error:&rowError];
            if (data) { [lines appendData:data]; [lines appendBytes:"\n" length:1]; }
            else serializationError = rowError.localizedDescription ?: @"snapshot JSON serialization failed";
            id label = Value(node, @"label");
            if ([label isKindOfClass:NSString.class] && [label length] && labeled < 15) {
                labeled++;
                log([NSString stringWithFormat:@"%@ labeled[%lu] elementType=%@ label=%@ frame=%@", prefix, (unsigned long)labeled, Value(node, @"elementType"), label, Value(node, @"frame")]);
            }
            id children = Value(node, @"children");
            if ([children isKindOfClass:NSArray.class]) {
                if (remote && depth >= MAX(1, maxDepth)) { if ([children count]) depthTruncated = YES; continue; }
                for (id child in [children reverseObjectEnumerator]) [pending addObject:@{@"node": child, @"depth": @(depth + 1)}];
            }
        }
        log([NSString stringWithFormat:@"%@ step walkSnapshot done %.0f ms nodes=%lu root label=%@ truncated=%@", prefix, (CFAbsoluteTimeGetCurrent()-walkStarted)*1000, (unsigned long)count, rootLabel, pending.count ? @"YES" : @"NO"]);
        [result addEntriesFromDictionary:@{@"nodeCount": @(count), @"truncated": @(pending.count > 0), @"rootLabel": rootLabel}];
        result[@"alerts"] = alerts; result[@"alertCount"] = @(alerts.count);
        if (remote) {
            return @{@"nodes": nodes, @"pid": @(pid), @"bundle": bundle ?: @"unknown",
                     @"truncated": @(pending.count > 0 || depthTruncated), @"snapshot_id": NSUUID.UUID.UUIDString};
        }
        if (serializationError) result[@"snapshotFileError"] = serializationError;
        else SaveSnapshot(lines, SnapshotString(rootLabel), pid, result);
        return result;
    } @catch (NSException *e) { result[@"error"] = Exception(e); return result; }
    @finally {
        if (!remote && !result[@"snapshotFile"] && !result[@"snapshotFileError"])
            result[@"snapshotFileError"] = result[@"error"] ?: @"snapshot traversal did not complete";
        result[@"elapsedMs"] = @((CFAbsoluteTimeGetCurrent()-started)*1000);
        log([NSString stringWithFormat:@"%@ result: %@", prefix, result]);
    }
}
+ (NSDictionary *)remotePIDAtPoint:(CGPoint)point {
    @try {
        id device = Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice");
        id client = Object(device, @"accessibilityInterface") ?: Object(NSClassFromString(@"XCAXClient_iOS"), @"sharedClient");
        Require(client, @"accessibilityElementForElementAtPoint:error:");
        NSError *error = nil;
        id hit = ((id (*)(id, SEL, CGPoint, NSError **))objc_msgSend)(client, NSSelectorFromString(@"accessibilityElementForElementAtPoint:error:"), point, &error);
        if (error) return @{@"pid": @0, @"error": [NSString stringWithFormat:@"%@: %ld: %@", error.domain, (long)error.code, error.localizedDescription]};
        if (!hit) return @{@"pid": @0, @"error": @"nil hit"};
        Require(hit, @"processIdentifier");
        int pid = ((int (*)(id, SEL))objc_msgSend)(hit, NSSelectorFromString(@"processIdentifier"));
        if (pid <= 0) return @{@"pid": @(pid), @"error": NSStringFromClass([hit class])};
        return @{@"pid": @(pid)};
    } @catch (NSException *e) { return @{@"pid": @0, @"error": Exception(e)}; }
}
// SpringBoard PID without traversing its accessibility tree.
+ (int)remoteSystemPID {
    @try {
        id device = Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice");
        id client = Object(device, @"accessibilityInterface") ?: Object(NSClassFromString(@"XCAXClient_iOS"), @"sharedClient");
        id system = Object(client, @"systemApplication");
        Require(system, @"processIdentifier");
        return ((int (*)(id, SEL))objc_msgSend)(system, NSSelectorFromString(@"processIdentifier"));
    } @catch (__unused NSException *e) { return 0; }
}
+ (nullable NSNumber *)remoteScreenLocked {
    // Same SpringBoardServices calls as WDA fb_isScreenLocked / IPURBridge isScreenLocked.
    typedef mach_port_t (*ServerPort)(void);
    typedef void (*LockStatus)(mach_port_t, BOOL *, BOOL *);
    static ServerPort serverPort;
    static LockStatus lockStatus;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        void *handle = dlopen(
            "/System/Library/PrivateFrameworks/SpringBoardServices.framework/SpringBoardServices", RTLD_LAZY);
        if (handle != NULL) {
            serverPort = (ServerPort)dlsym(handle, "SBSSpringBoardServerPort");
            lockStatus = (LockStatus)dlsym(handle, "SBGetScreenLockStatus");
        }
    });
    if (serverPort == NULL || lockStatus == NULL) return nil;
    BOOL locked = NO;
    BOOL passcodeEnabled = NO;
    lockStatus(serverPort(), &locked, &passcodeEnabled);
    return @(locked);
}
+ (NSDictionary *)remoteForeground {
    @try {
        id device = Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice");
        id client = Object(device, @"accessibilityInterface") ?: Object(NSClassFromString(@"XCAXClient_iOS"), @"sharedClient");
        id system = Object(client, @"systemApplication");
        id selected = nil;
        for (id element in Object(client, @"activeApplications")) { if (![element isEqual:system]) { selected = element; break; } }
        selected = selected ?: system;
        Require(selected, @"processIdentifier");
        int pid = ((int (*)(id, SEL))objc_msgSend)(selected, NSSelectorFromString(@"processIdentifier"));
        NSString *bundle = nil;
        for (NSString *source in @[@"applicationProcessTracker", @"applicationMonitor"]) {
            id provider = Object([source isEqualToString:@"applicationProcessTracker"] ? client : device, source);
            if (![provider respondsToSelector:NSSelectorFromString(@"applicationProcessWithPID:")]) continue;
            id process = ((id (*)(id, SEL, int))objc_msgSend)(provider, NSSelectorFromString(@"applicationProcessWithPID:"), pid);
            bundle = Object(process, @"bundleID");
            if ([bundle isKindOfClass:NSString.class] && bundle.length) break;
        }
        return @{@"pid": @(pid), @"bundle": bundle ?: @"unknown"};
    } @catch (__unused NSException *e) { return @{@"pid": @0, @"bundle": @"unknown"}; }
}
+ (NSData *)screenshotWithLog:(void (^)(NSString *))log error:(NSString **)error {
    // Called on the worker queue; watchdog reports a hang without releasing the gate.
    @try {
        id device = STEP(@"screenshot", @"sharedDevice", Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice"));
        id source = STEP(@"screenshot", @"screenDataSource", Object(device, @"screenDataSource"));
        id screen = STEP(@"screenshot", @"mainScreen", Object(NSClassFromString(@"XCUIScreen"), @"mainScreen"));
        Require(screen, @"displayID");
        long long display = STEP(@"screenshot", @"displayID", ((long long (*)(id, SEL))objc_msgSend)(screen, NSSelectorFromString(@"displayID")));
        Class encodingClass = NSClassFromString(@"XCTImageEncoding");
        Require([encodingClass alloc], @"initWithUniformTypeIdentifier:compressionQuality:");
        id encoding = STEP(@"screenshot", @"initWithUniformTypeIdentifier:compressionQuality:", ((id (*)(id, SEL, id, double))objc_msgSend)([encodingClass alloc], NSSelectorFromString(@"initWithUniformTypeIdentifier:compressionQuality:"), @"public.png", 1.0));
        Class requestClass = NSClassFromString(@"XCTScreenshotRequest");
        Require([requestClass alloc], @"initWithScreenID:rect:encoding:options:");
        id request = STEP(@"screenshot", @"initWithScreenID:rect:encoding:options:", ((id (*)(id, SEL, long long, CGRect, id, unsigned long long))objc_msgSend)([requestClass alloc], NSSelectorFromString(@"initWithScreenID:rect:encoding:options:"), display, CGRectNull, encoding, 0ULL));
        Require(source, @"requestScreenshotWithRequest:withReply:");
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        NSObject *lock = [NSObject new];
        __block NSData *data; __block NSString *failure;
        void (^reply)(id, NSError *) = ^(id image, NSError *e) {
            @synchronized(lock) { data = STEP(@"screenshot", @"data", Object(image, @"data")); failure = e.localizedDescription; }
            dispatch_semaphore_signal(done);
        };
        CFAbsoluteTime requestStart = CFAbsoluteTimeGetCurrent();
        log(@"screenshot step requestScreenshotWithRequest:withReply: start");
        ((void (*)(id, SEL, id, id))objc_msgSend)(source, NSSelectorFromString(@"requestScreenshotWithRequest:withReply:"), request, reply);
        log([NSString stringWithFormat:@"screenshot step requestScreenshotWithRequest:withReply: done %.0f ms (request dispatched)", (CFAbsoluteTimeGetCurrent()-requestStart)*1000]);
        // Keep the execution gate held until the daemon callback has actually returned.
        STEP(@"screenshot", @"replyWait", dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER));
        @synchronized(lock) {
            if (![data isKindOfClass:NSData.class]) { if (error) *error = failure ?: @"screenshot returned no bytes"; return nil; }
            return data;
        }
    } @catch (NSException *e) { if (error) *error = Exception(e); return nil; }
}
+ (void)tapTarget:(UIButton *)target worker:(dispatch_queue_t)worker log:(void (^)(NSString *))log completion:(void (^)(NSString *))completion {
    NSAssert(NSThread.isMainThread, @"target capture on main");
    UIWindow *window = target.window;
    CGRect rect = [target convertRect:target.bounds toView:window];
    CGPoint center = CGPointMake(CGRectGetMidX(rect), CGRectGetMidY(rect));
    UIView *hit = [window hitTest:center withEvent:nil];
    if (!window || UIApplication.sharedApplication.applicationState != UIApplicationStateActive ||
        !target.enabled || target.hidden || target.alpha < 0.9 || !CGRectContainsRect(window.bounds, rect) ||
        !(hit == target || [hit isDescendantOfView:target]) || window.windowScene.interfaceOrientation != UIInterfaceOrientationPortrait) {
        completion(@"tap refused: target must be visible, uncovered, active and portrait"); return;
    }
    CGPoint point = [window convertPoint:center toCoordinateSpace:window.screen.coordinateSpace];
    dispatch_async(worker, ^{
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Require([recordClass alloc], @"initWithName:interfaceOrientation:");
        id record = STEP(@"tap", @"initWithName:interfaceOrientation:", ((id (*)(id, SEL, id, long long))objc_msgSend)([recordClass alloc], NSSelectorFromString(@"initWithName:interfaceOrientation:"), @"self-runner-target-only", 1LL));
        Require(record, @"setTargetProcessID:");
        VoidStep(log, @"setTargetProcessID:", ^{ ((void (*)(id, SEL, long long))objc_msgSend)(record, NSSelectorFromString(@"setTargetProcessID:"), (long long)getpid()); });
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        Require([pathClass alloc], @"initForTouchAtPoint:offset:");
        id path = STEP(@"tap", @"initForTouchAtPoint:offset:", ((id (*)(id, SEL, CGPoint, double))objc_msgSend)([pathClass alloc], NSSelectorFromString(@"initForTouchAtPoint:offset:"), point, 0.0));
        Require(path, @"liftUpAtOffset:");
        VoidStep(log, @"liftUpAtOffset:", ^{ ((void (*)(id, SEL, double))objc_msgSend)(path, NSSelectorFromString(@"liftUpAtOffset:"), 0.05); });
        Require(record, @"addPointerEventPath:");
        VoidStep(log, @"addPointerEventPath:", ^{ ((void (*)(id, SEL, id))objc_msgSend)(record, NSSelectorFromString(@"addPointerEventPath:"), path); });
        Require(record, @"synthesizeWithError:");
        // Worker queue keeps the app main loop able to receive the synthetic target action.
        {
            NSString *failure;
            @try {
                NSError *error = nil;
                BOOL ok;
                CFAbsoluteTime synthesisStart = CFAbsoluteTimeGetCurrent();
                log(@"tap step synthesizeWithError: start");
                @try {
                    ok = ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(record, NSSelectorFromString(@"synthesizeWithError:"), &error);
                } @finally {
                    log([NSString stringWithFormat:@"tap step synthesizeWithError: finished %.0f ms", (CFAbsoluteTimeGetCurrent()-synthesisStart)*1000]);
                }
                failure = ok ? nil : (error.localizedDescription ?: @"tap synthesis failed");
            } @catch (NSException *e) { failure = Exception(e); }
            dispatch_async(dispatch_get_main_queue(), ^{ completion(failure); });
        }
    } @catch (NSException *e) { dispatch_async(dispatch_get_main_queue(), ^{ completion(Exception(e)); }); }
    });
}
+ (nullable NSString *)tapScreenPoint:(CGPoint)point pid:(int)pid log:(void (^)(NSString *))log {
    // Caller validates the target owner and orientation before entering synthesis.
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Require([recordClass alloc], @"initWithName:interfaceOrientation:");
        id record = STEP(@"offer-tap", @"initWithName:interfaceOrientation:", ((id (*)(id, SEL, id, long long))objc_msgSend)([recordClass alloc], NSSelectorFromString(@"initWithName:interfaceOrientation:"), @"self-runner-offer", 1LL));
        Require(record, @"setTargetProcessID:");
        VoidStep(log, @"setTargetProcessID:", ^{ ((void (*)(id, SEL, long long))objc_msgSend)(record, NSSelectorFromString(@"setTargetProcessID:"), (long long)pid); });
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        Require([pathClass alloc], @"initForTouchAtPoint:offset:");
        id path = STEP(@"offer-tap", @"initForTouchAtPoint:offset:", ((id (*)(id, SEL, CGPoint, double))objc_msgSend)([pathClass alloc], NSSelectorFromString(@"initForTouchAtPoint:offset:"), point, 0.0));
        VoidStep(log, @"liftUpAtOffset:", ^{ ((void (*)(id, SEL, double))objc_msgSend)(path, NSSelectorFromString(@"liftUpAtOffset:"), 0.05); });
        VoidStep(log, @"addPointerEventPath:", ^{ ((void (*)(id, SEL, id))objc_msgSend)(record, NSSelectorFromString(@"addPointerEventPath:"), path); });
        Require(record, @"synthesizeWithError:");
        NSError *error = nil;
        BOOL ok = ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(record, NSSelectorFromString(@"synthesizeWithError:"), &error);
        return ok ? nil : (error.localizedDescription ?: @"tap synthesis failed");
    } @catch (NSException *e) { return Exception(e); }
}
+ (nullable NSString *)swipeScreenFrom:(CGPoint)point to:(CGPoint)destination pid:(int)pid log:(void (^)(NSString *))log {
    return [self swipeScreenFrom:point to:destination pid:pid duration:0.2 log:log];
}
+ (nullable NSString *)swipeScreenFrom:(CGPoint)point to:(CGPoint)destination pid:(int)pid duration:(double)duration {
    return [self swipeScreenFrom:point to:destination pid:pid duration:duration log:^(__unused NSString *message) {}];
}
+ (nullable NSString *)swipeScreenFrom:(CGPoint)point to:(CGPoint)destination pid:(int)pid duration:(double)duration log:(void (^)(NSString *))log {
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Require([recordClass alloc], @"initWithName:interfaceOrientation:");
        id record = STEP(@"offer-tap", @"initWithName:interfaceOrientation:", ((id (*)(id, SEL, id, long long))objc_msgSend)([recordClass alloc], NSSelectorFromString(@"initWithName:interfaceOrientation:"), @"self-runner-dismiss-banner", 1LL));
        Require(record, @"setTargetProcessID:");
        VoidStep(log, @"setTargetProcessID:", ^{ ((void (*)(id, SEL, long long))objc_msgSend)(record, NSSelectorFromString(@"setTargetProcessID:"), (long long)pid); });
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        Require([pathClass alloc], @"initForTouchAtPoint:offset:");
        id path = STEP(@"offer-tap", @"initForTouchAtPoint:offset:", ((id (*)(id, SEL, CGPoint, double))objc_msgSend)([pathClass alloc], NSSelectorFromString(@"initForTouchAtPoint:offset:"), point, 0.0));
        Require(path, @"moveToPoint:atOffset:");
        Require(path, @"liftUpAtOffset:");
        Require(record, @"addPointerEventPath:");
        ((void (*)(id, SEL, CGPoint, double))objc_msgSend)(path, NSSelectorFromString(@"moveToPoint:atOffset:"), destination, duration * 0.9);
        VoidStep(log, @"liftUpAtOffset:", ^{ ((void (*)(id, SEL, double))objc_msgSend)(path, NSSelectorFromString(@"liftUpAtOffset:"), duration); });
        VoidStep(log, @"addPointerEventPath:", ^{ ((void (*)(id, SEL, id))objc_msgSend)(record, NSSelectorFromString(@"addPointerEventPath:"), path); });
        Require(record, @"synthesizeWithError:");
        NSError *error = nil;
        BOOL ok = ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(record, NSSelectorFromString(@"synthesizeWithError:"), &error);
        return ok ? nil : (error.localizedDescription ?: @"tap synthesis failed");
    } @catch (NSException *e) { return Exception(e); }
}
// All errors are fixed protocol codes: private exceptions can contain input text/URLs.
static NSString *RemoteExceptionCode(NSException *e, NSString *failure) {
    return [@[@"MissingPrivateAPI", @"UnsupportedPrivateABI"] containsObject:e.name] ? @"unsupported" : failure;
}
+ (nullable NSString *)remoteTypeText:(NSString *)text pid:(int)pid {
    @try {
        void (^quiet)(NSString *) = ^(__unused NSString *message) {};
        id allocated = [NSClassFromString(@"XCSynthesizedEventRecord") alloc];
        CheckedSignature(allocated, @"initWithName:", @encode(id), @[@(@encode(id))], quiet);
        id record = ((id (*)(id, SEL, id))objc_msgSend)(allocated, NSSelectorFromString(@"initWithName:"), @"self-runner-type");
        id path = CheckedObject([NSClassFromString(@"XCPointerEventPath") alloc], @"initForTextInput", quiet);
        CheckedSignature(record, @"setTargetProcessID:", @encode(void), @[@(@encode(long long))], quiet);
        ((void (*)(id, SEL, long long))objc_msgSend)(record, NSSelectorFromString(@"setTargetProcessID:"), (long long)pid);
        CheckedSignature(path, @"typeText:atOffset:typingSpeed:shouldRedact:", @encode(void),
                         @[@(@encode(id)), @(@encode(double)), @(@encode(unsigned long long)), @(@encode(BOOL))], quiet);
        ((void (*)(id, SEL, id, double, unsigned long long, BOOL))objc_msgSend)(path,
            NSSelectorFromString(@"typeText:atOffset:typingSpeed:shouldRedact:"), text, 0.0, 60ULL, YES);
        Require(record, @"addPointerEventPath:");
        ((void (*)(id, SEL, id))objc_msgSend)(record, NSSelectorFromString(@"addPointerEventPath:"), path);
        Require(record, @"synthesizeWithError:");
        NSError *error = nil;
        return ((BOOL (*)(id, SEL, NSError **))objc_msgSend)(record, NSSelectorFromString(@"synthesizeWithError:"), &error) ? nil : @"type_failed";
    } @catch (NSException *e) { return RemoteExceptionCode(e, @"type_failed"); }
}
+ (nullable NSString *)remotePressButton:(NSString *)button {
    @try {
        id device = Object(NSClassFromString(@"XCUIDevice"), @"sharedDevice");
        if ([button isEqualToString:@"lock"]) {
            // Same private device API used by WebDriverAgent's /wda/lock.
            SEL lock = NSSelectorFromString(@"pressLockButton");
            if (![device respondsToSelector:lock]) return @"unsupported";
            CheckedSignature(device, @"pressLockButton", @encode(void), @[], ^(__unused NSString *message) {});
            ((void (*)(id, SEL))objc_msgSend)(device, lock);
            return nil;
        }
        CheckedSignature(device, @"pressButton:", @encode(void), @[@(@encode(long long))], ^(__unused NSString *message) {});
        // XCUIDeviceButton: home=1, volumeUp=2, volumeDown=3.
        long long value = [button isEqualToString:@"home"] ? 1 : ([button isEqualToString:@"volume_up"] ? 2 : 3);
        ((void (*)(id, SEL, long long))objc_msgSend)(device, NSSelectorFromString(@"pressButton:"), value);
        return nil;
    } @catch (NSException *e) { return RemoteExceptionCode(e, @"press_failed"); }
}
+ (NSDictionary *)remoteLockEntry:(NSString *)entry strategy:(NSString *)strategy {
    NSTimeInterval start = NSProcessInfo.processInfo.systemUptime;
    NSMutableDictionary *attempt = [@{@"strategy": strategy, @"entry": entry} mutableCopy];
    @try {
        void (^quiet)(NSString *) = ^(__unused NSString *message) {};
        id device = CheckedObject(NSClassFromString(@"XCUIDevice"), @"sharedDevice", quiet);
        if ([entry isEqualToString:@"pressLockButton"]) {
            CheckedSignature(device, entry, @encode(void), @[], quiet);
            ((void (*)(id, SEL))objc_msgSend)(device, NSSelectorFromString(entry));
        } else if ([entry isEqualToString:@"_dispatchEventWithPage:usage:duration:"]) {
            CheckedSignature(device, entry, @encode(void), @[@(@encode(unsigned int)), @(@encode(unsigned int)), @(@encode(double))], quiet);
            ((void (*)(id, SEL, unsigned int, unsigned int, double))objc_msgSend)(device, NSSelectorFromString(entry), 0x0C, 0x30, 0.1);
        } else if ([entry isEqualToString:@"performDeviceEvent:error:"] ||
                   [entry isEqualToString:@"_XCT_performDeviceEvent:completion:"]) {
            id target = device;
            if ([entry isEqualToString:@"_XCT_performDeviceEvent:completion:"]) {
                id session = CheckedObject(NSClassFromString(@"XCTRunnerDaemonSession"), @"sharedSession", quiet);
                target = CheckedObject(session, @"daemonProxy", quiet);
                CheckedSignature(target, entry, @encode(void), @[@"@", @"@?"], quiet);
            } else {
                CheckedSignature(target, entry, @encode(BOOL), @[@"@", @"^@"], quiet);
            }
            NSString *factory = @"deviceEventWithPage:usage:duration:";
            Class eventClass = NSClassFromString(@"XCDeviceEvent");
            CheckedSignature(eventClass, factory, @encode(id), @[@(@encode(unsigned int)), @(@encode(unsigned int)), @(@encode(double))], quiet);
            id event = ((id (*)(id, SEL, unsigned int, unsigned int, double))objc_msgSend)(eventClass, NSSelectorFromString(factory), 0x0C, 0x30, 0.1);
            if (!event) {
                attempt[@"error_code"] = @"event_failed";
            } else if ([entry isEqualToString:@"performDeviceEvent:error:"]) {
                NSError *error = nil;
                BOOL ok = ((BOOL (*)(id, SEL, id, NSError **))objc_msgSend)(target, NSSelectorFromString(entry), event, &error);
                if (!ok || error) attempt[@"error_code"] = @"press_failed";
            } else {
                dispatch_semaphore_t done = dispatch_semaphore_create(0);
                NSObject *guard = [NSObject new];
                __block BOOL failed = NO;
                void (^completion)(NSError *) = ^(NSError *error) {
                    @synchronized(guard) { failed = error != nil; }
                    dispatch_semaphore_signal(done);
                };
                ((void (*)(id, SEL, id, id))objc_msgSend)(target, NSSelectorFromString(entry), event, completion);
                if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC)) != 0) {
                    attempt[@"error_code"] = @"timeout";
                } else {
                    @synchronized(guard) { if (failed) attempt[@"error_code"] = @"press_failed"; }
                }
            }
        } else {
            attempt[@"error_code"] = @"unsupported";
        }
    } @catch (NSException *e) {
        attempt[@"error_code"] = RemoteExceptionCode(e, @"press_failed");
    }
    attempt[@"ms"] = @((NSProcessInfo.processInfo.systemUptime - start) * 1000);
    return attempt;
}
+ (nullable NSString *)remoteLaunchBundle:(NSString *)bundle {
    @try {
        id workspace = Object(NSClassFromString(@"LSApplicationWorkspace"), @"defaultWorkspace");
        CheckedSignature(workspace, @"openApplicationWithBundleID:", @encode(BOOL), @[@(@encode(id))], ^(__unused NSString *message) {});
        return ((BOOL (*)(id, SEL, id))objc_msgSend)(workspace, NSSelectorFromString(@"openApplicationWithBundleID:"), bundle) ? nil : @"launch_failed";
    } @catch (NSException *e) { return RemoteExceptionCode(e, @"launch_failed"); }
}
+ (void)reset {
    NSAssert(NSThread.isMainThread, @"reset on main");
    generation++;
    // Cancel this bootstrap's continuations, preserving the process-wide shared session.
    // Never disable automation mode or invalidate its connection on Stop.
}
@end
