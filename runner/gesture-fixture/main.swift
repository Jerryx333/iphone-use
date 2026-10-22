// Simulator-only touch receiver. Counters are reset only by a fresh launch.
import UIKit

final class TouchPad: UIView {
  private let result = UILabel()
  private let runID = ProcessInfo.processInfo.environment["GESTURE_RUN_ID"] ?? "missing"
  private var downs = 0, ups = 0, cancels = 0, maxFingers = 0
  private var start: CGPoint?, end: CGPoint?
  private var beganAt: TimeInterval?
  private var firstMotionAt: TimeInterval?
  private var edgeBeganAt: TimeInterval?
  private var hover: TimeInterval = 0
  private var elapsed: TimeInterval = 0
  private var visits: [String] = []

  override init(frame: CGRect) {
    super.init(frame: frame)
    backgroundColor = .systemBackground
    isMultipleTouchEnabled = true
    result.numberOfLines = 0
    result.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
    result.accessibilityIdentifier = "gesture-result"
    result.isUserInteractionEnabled = false
    addSubview(result)
  }
  required init?(coder: NSCoder) { fatalError("init(coder:) is unused") }

  override func layoutSubviews() {
    super.layoutSubviews()
    result.frame = CGRect(x: 16, y: safeAreaInsets.top + 12, width: bounds.width - 32, height: 190)
    publish()
  }
  override func draw(_ rect: CGRect) {
    super.draw(rect)
    for (x, y) in [(0.3, 0.4), (0.13, 0.5), (0.5, 0.4)] {
      let p = CGPoint(x: bounds.width * x, y: bounds.height * y)
      UIColor.systemBlue.setFill()
      UIBezierPath(ovalIn: CGRect(x: p.x - 5, y: p.y - 5, width: 10, height: 10)).fill()
    }
  }
  private func normalize(_ p: CGPoint) -> [Double] {
    [Double(p.x / max(bounds.width, 1)), Double(p.y / max(bounds.height, 1))]
  }
  private func sample(_ touch: UITouch, ending: Bool = false) {
    let p = touch.location(in: self)
    if firstMotionAt == nil, let start, hypot(p.x - start.x, p.y - start.y) > 2.5 {
      firstMotionAt = touch.timestamp
    }
    let edge = CGPoint(x: bounds.width * 0.13, y: bounds.height * 0.5)
    let inside = hypot(p.x - edge.x, p.y - edge.y) <= 2.5
    if inside {
      if edgeBeganAt == nil { edgeBeganAt = touch.timestamp }
      hover = max(hover, touch.timestamp - (edgeBeganAt ?? touch.timestamp))
    } else if let entered = edgeBeganAt {
      hover = max(hover, touch.timestamp - entered)
      edgeBeganAt = nil
    }
    for (name, x, y) in [("start", 0.3, 0.4), ("edge", 0.13, 0.5), ("end", 0.5, 0.4)] {
      if hypot(p.x - bounds.width * x, p.y - bounds.height * y) <= 4,
         visits.last != name { visits.append(name) }
    }
    if let beganAt { elapsed = max(elapsed, touch.timestamp - beganAt) }
    if ending { end = p; edgeBeganAt = nil }
  }
  private func publish() {
    let record: [String: Any] = [
      "run_id": runID, "down_count": downs, "up_count": ups,
      "cancel_count": cancels, "max_fingers": maxFingers,
      "hover_ms": Int(hover * 1000), "duration_ms": Int(elapsed * 1000),
      "initial_hold_ms": firstMotionAt.map { Int(($0 - (beganAt ?? $0)) * 1000) } as Any? ?? NSNull(),
      "start": start.map(normalize) as Any? ?? NSNull(),
      "end": end.map(normalize) as Any? ?? NSNull(),
      "visits": visits, "width": Double(bounds.width), "height": Double(bounds.height),
      "screen_width": Double(window?.screen.bounds.width ?? 0),
      "screen_height": Double(window?.screen.bounds.height ?? 0),
      "native_width": Double(window?.screen.nativeBounds.width ?? 0),
      "native_height": Double(window?.screen.nativeBounds.height ?? 0),
      "screen_scale": Double(window?.screen.scale ?? 0),
    ]
    if let data = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]),
       let text = String(data: data, encoding: .utf8) {
      result.text = text
      result.accessibilityLabel = text
    }
  }
  override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
    downs += touches.count
    maxFingers = max(maxFingers, event?.allTouches?.filter { ![.ended, .cancelled].contains($0.phase) }.count ?? touches.count)
    for touch in touches {
      if beganAt == nil { beganAt = touch.timestamp; start = touch.location(in: self) }
      sample(touch)
    }
    publish()
  }
  override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
    for touch in touches {
      for sampleTouch in event?.coalescedTouches(for: touch) ?? [touch] { sample(sampleTouch) }
    }
    publish()
  }
  override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
    ups += touches.count
    for touch in touches { sample(touch, ending: true) }
    publish()
  }
  override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
    cancels += touches.count
    for touch in touches { sample(touch, ending: true) }
    publish()
  }
}
final class PadController: UIViewController {
  override func loadView() { view = TouchPad(frame: .zero) }
  override var prefersStatusBarHidden: Bool { true }
  override var prefersHomeIndicatorAutoHidden: Bool { true }
}
@objc(GestureFixtureSceneDelegate)
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
  var window: UIWindow?
  func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
    guard let scene = scene as? UIWindowScene else { return }
    let window = UIWindow(windowScene: scene)
    window.rootViewController = PadController()
    window.makeKeyAndVisible()
    self.window = window
  }
}
final class AppDelegate: UIResponder, UIApplicationDelegate {}
UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(AppDelegate.self))
