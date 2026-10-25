// The on-phone way out: a notification with an End button while the runner serves.
//
// iOS gives no app a button over other apps, and its own way out of UI automation (holding both
// volume keys) also changes the volume. A notification stays on the Lock Screen and in
// Notification Center, so the person holding the phone can end the session from there: the
// button ends the serve loop like POST /shutdown, the runner exits, and the Mac supervisor
// parks instead of starting it again.
//
// IPU_RUNNER_EXIT_NOTICE=0 turns it off.

import Foundation
import UserNotifications

final class RunnerExitNotice: NSObject, UNUserNotificationCenterDelegate {
  static let categoryID = "ipu.session"
  static let endActionID = "ipu.session.end"
  static let requestID = "ipu.session"

  /// Called with `true` when the tap brought the (blank) runner app forward.
  private let onEnd: (_ broughtForward: Bool) -> Void

  init(onEnd: @escaping (_ broughtForward: Bool) -> Void) {
    self.onEnd = onEnd
  }

  private static var chinese: Bool {
    Locale.preferredLanguages.first?.hasPrefix("zh") ?? false
  }

  func post() {
    let center = UNUserNotificationCenter.current()
    center.delegate = self
    let end = UNNotificationAction(
      identifier: Self.endActionID,
      title: Self.chinese ? "结束控制" : "End control",
      options: [.destructive])
    center.setNotificationCategories([
      UNNotificationCategory(
        identifier: Self.categoryID, actions: [end], intentIdentifiers: [], options: [])
    ])
    center.requestAuthorization(options: [.alert]) { granted, error in
      NSLog("ipu-runner: exit notice authorization %@%@", granted ? "granted" : "denied",
            error.map { " (\($0.localizedDescription))" } ?? "")
      guard granted else { return }
      let content = UNMutableNotificationContent()
      content.title = Self.chinese ? "iPhone Use 正在控制这台手机" : "iPhone Use is controlling this iPhone"
      content.body = Self.chinese
        ? "长按这条通知，点「结束控制」把手机还给你；电脑不会自动重连。"
        : "Touch and hold this notification, then tap End control to take the phone back. The Mac will not reconnect on its own."
      content.categoryIdentifier = Self.categoryID
      content.threadIdentifier = Self.categoryID
      let request = UNNotificationRequest(identifier: Self.requestID, content: content, trigger: nil)
      center.add(request) { error in
        NSLog("ipu-runner: exit notice %@", error.map { "not posted: \($0.localizedDescription)" } ?? "posted")
      }
    }
  }

  func remove() {
    let center = UNUserNotificationCenter.current()
    center.removeDeliveredNotifications(withIdentifiers: [Self.requestID])
    center.removePendingNotificationRequests(withIdentifiers: [Self.requestID])
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    completionHandler([.banner, .list])
  }

  /// The End button, or a tap on the notification itself (which would otherwise only bring the
  /// blank runner app forward).
  func userNotificationCenter(
    _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    let action = response.actionIdentifier
    if action == Self.endActionID || action == UNNotificationDefaultActionIdentifier {
      NSLog("ipu-runner: session ended from the notification (%@)", action)
      onEnd(action == UNNotificationDefaultActionIdentifier)
    }
    completionHandler()
  }
}
