import UIKit
import CryptoKit

final class ValueController: UIViewController, UITextViewDelegate {
  let long = UITextView()
  var fields: [UITextField] = []
  let flag = UISwitch()
  var events = 0
  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .white
    let stack = UIStackView(); stack.axis = .vertical; stack.spacing = 8
    stack.translatesAutoresizingMaskIntoConstraints = false; view.addSubview(stack)
    NSLayoutConstraint.activate([
      stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
      stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
      stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16)
    ])
    long.text = "value-root-" + String(repeating: "字", count: 6015) + "🙂"
    long.accessibilityIdentifier = "complete-field"; long.accessibilityLabel = "Complete field"
    long.font = .systemFont(ofSize: 14); long.delegate = self
    long.heightAnchor.constraint(equalToConstant: 120).isActive = true; stack.addArrangedSubview(long)
    func field(_ identifier: String, _ text: String, label: String? = nil, secure: Bool = false) {
      let field = UITextField(); field.borderStyle = .roundedRect
      field.accessibilityIdentifier = identifier; field.accessibilityLabel = label ?? identifier
      field.isSecureTextEntry = secure; field.text = text
      field.heightAnchor.constraint(equalToConstant: 36).isActive = true
      field.addTarget(self, action: #selector(changed), for: [.editingChanged, .editingDidBegin, .editingDidEnd])
      fields.append(field); stack.addArrangedSubview(field)
    }
    field("emoji-field", "🙂abc")
    field("quoted-field", "quoted-value", label: "Quote ' \\ 100%@ field\nsecond\tline")
    field("secure-field", "OWNED_SYNTHETIC_SECRET", secure: true)
    field("duplicate-a", "one", label: "Duplicate")
    field("duplicate-b", "two", label: "Duplicate")
    flag.isOn = true; flag.accessibilityIdentifier = "flag-field"
    flag.addTarget(self, action: #selector(changed), for: .valueChanged); stack.addArrangedSubview(flag)
  }
  override func viewDidAppear(_ animated: Bool) { super.viewDidAppear(animated); save() }
  @objc func changed() { events += 1; save() }
  func textViewDidChange(_ textView: UITextView) { changed() }
  func textViewDidBeginEditing(_ textView: UITextView) { changed() }
  func textViewDidEndEditing(_ textView: UITextView) { changed() }
  func save() {
    let text = long.text ?? ""
    let sha = SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    let state: [String: Any] = ["fixture_variant": "scene", "bundle_version": Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "", "pid": ProcessInfo.processInfo.processIdentifier,
      "events": events, "focused": long.isFirstResponder || fields.contains(where: { $0.isFirstResponder }),
      "sha256": sha, "utf16_length": text.utf16.count, "flag": flag.isOn]
    let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("value-state.json")
    if let data = try? JSONSerialization.data(withJSONObject: state) { try? data.write(to: url, options: .atomic) }
  }
}
final class ValueAppDelegate: UIResponder, UIApplicationDelegate {
  func application(_ application: UIApplication, didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool { true }
  func application(_ application: UIApplication, configurationForConnecting session: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
    let configuration = UISceneConfiguration(name: "Value Fixture", sessionRole: session.role)
    configuration.delegateClass = ValueSceneDelegate.self
    return configuration
  }
}
final class ValueSceneDelegate: UIResponder, UIWindowSceneDelegate {
  var window: UIWindow?
  func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
    guard let scene = scene as? UIWindowScene else { return }
    let window = UIWindow(windowScene: scene)
    window.rootViewController = ValueController(); window.makeKeyAndVisible(); self.window = window
  }
}
UIApplicationMain(CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(ValueAppDelegate.self))
