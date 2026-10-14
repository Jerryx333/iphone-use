#import <UIKit/UIKit.h>
NS_ASSUME_NONNULL_BEGIN
@interface SpikeBridge : NSObject
+ (void)loadXCTestWithLog:(void (^)(NSString *))log completion:(void (^)(NSString *_Nullable))completion;
+ (nullable NSString *)enableAutomation;
+ (nullable NSNumber *)automationModeEnabledWithLog:(void (^)(NSString *))log;
+ (BOOL)needsRelaunch;
+ (NSDictionary *)inspectUIAutomationWithLog:(void (^)(NSString *))log;
+ (NSDictionary *)loadAccessibilityWithLog:(void (^)(NSString *))log;
+ (NSDictionary *)loadAccessibilityVariant:(NSString *)variant log:(void (^)(NSString *))log;
+ (NSArray<NSString *> *)installQuiescenceBypass;
+ (NSDictionary *)readScreenWithMaxDepth:(NSInteger)maxDepth runnerParameters:(BOOL)runnerParameters log:(void (^)(NSString *))log NS_SWIFT_NAME(readScreen(maxDepth:runnerParameters:log:));
+ (nullable NSData *)screenshotWithLog:(void (^)(NSString *))log error:(NSString *_Nullable *_Nullable)error NS_SWIFT_NAME(screenshot(log:error:));
// Captures the live button center on main; refuses background, covered, detached or disabled targets.
+ (void)tapTarget:(UIButton *)target worker:(dispatch_queue_t)worker log:(void (^)(NSString *))log completion:(void (^)(NSString *_Nullable))completion;
+ (nullable NSString *)tapScreenPoint:(CGPoint)point pid:(int)pid log:(void (^)(NSString *))log NS_SWIFT_NAME(tapScreen(point:pid:log:));
+ (NSDictionary *)remoteReadWithMaxDepth:(NSInteger)maxDepth NS_SWIFT_NAME(remoteRead(maxDepth:));
+ (NSDictionary *)remoteSystemReadWithMaxDepth:(NSInteger)maxDepth NS_SWIFT_NAME(remoteSystemRead(maxDepth:));
+ (nullable NSString *)swipeScreenFrom:(CGPoint)point to:(CGPoint)destination pid:(int)pid log:(void (^)(NSString *))log NS_SWIFT_NAME(swipeScreen(from:to:pid:log:));
+ (NSDictionary *)remoteForeground;
+ (int)remoteSystemPID;
+ (nullable NSNumber *)remoteScreenLocked NS_SWIFT_NAME(remoteScreenLocked());
+ (NSDictionary *)remoteInputReadWithMaxDepth:(NSInteger)maxDepth NS_SWIFT_NAME(remoteInputRead(maxDepth:));
+ (nullable NSString *)remoteTypeText:(NSString *)text pid:(int)pid NS_SWIFT_NAME(remoteType(text:pid:));
+ (nullable NSString *)remotePressButton:(NSString *)button NS_SWIFT_NAME(remotePress(button:));
+ (NSDictionary *)remoteLockEntry:(NSString *)entry strategy:(NSString *)strategy NS_SWIFT_NAME(remoteLock(entry:strategy:));
+ (nullable NSString *)remoteLaunchBundle:(NSString *)bundle NS_SWIFT_NAME(remoteLaunch(bundle:));
+ (nullable NSString *)swipeScreenFrom:(CGPoint)point to:(CGPoint)destination pid:(int)pid duration:(double)duration NS_SWIFT_NAME(swipeScreen(from:to:pid:duration:));
+ (NSDictionary *)remotePIDAtPoint:(CGPoint)point NS_SWIFT_NAME(remotePID(at:));
+ (void)reset;
@end
NS_ASSUME_NONNULL_END
