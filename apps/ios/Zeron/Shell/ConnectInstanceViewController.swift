import UIKit

/// Native pairing keeps keys out of URLs, logs and UserDefaults.
final class ConnectInstanceViewController: UIViewController {
    private let app: AppModel
    private let address = UITextField()
    private let key = UITextField()
    private let status = UILabel()
    private let connect = UIButton(type: .system)
    private var task: Task<Void, Never>?

    init(app: AppModel) { self.app = app; super.init(nibName: nil, bundle: nil) }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        if isBeingDismissed || navigationController?.isBeingDismissed == true {
            task?.cancel()
        }
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Connect to Instance"
        view.backgroundColor = Palette.background
        navigationItem.leftBarButtonItem = UIBarButtonItem(systemItem: .cancel, primaryAction: UIAction { [weak self] _ in
            self?.task?.cancel()
            self?.dismiss(animated: true)
        })
        address.placeholder = "https://computer.example.com"
        address.keyboardType = .URL
        address.textContentType = .URL
        address.accessibilityLabel = "Instance address"
        address.accessibilityIdentifier = "instance-address"
        key.placeholder = "One-time pairing key"
        key.isSecureTextEntry = true
        key.accessibilityLabel = "Pairing key"
        key.accessibilityIdentifier = "instance-key"
        for field in [address, key] {
            field.borderStyle = .roundedRect
            field.autocapitalizationType = .none
            field.autocorrectionType = .no
            field.spellCheckingType = .no
        }
        let explanation = UILabel()
        explanation.text = "Enter your computer’s HTTPS address and the pairing key generated on it. Your agents and files stay on that computer."
        explanation.font = Fonts.ui(.sans, 16)
        explanation.textColor = Palette.secondary
        explanation.numberOfLines = 0
        status.numberOfLines = 0
        status.textColor = Palette.danger
        status.font = Fonts.ui(.sans, 14)
        var style = UIButton.Configuration.filled()
        style.title = "Connect"
        style.cornerStyle = .capsule
        connect.configuration = style
        connect.accessibilityIdentifier = "pair-instance"
        connect.addAction(UIAction { [weak self] _ in self?.pair() }, for: .touchUpInside)
        let stack = UIStackView(arrangedSubviews: [explanation, address, key, status, connect])
        stack.axis = .vertical
        stack.spacing = 20
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -24),
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 24),
        ])
    }

    private func pair() {
        guard task == nil else { return }
        let endpoint = address.text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let code = key.text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !endpoint.isEmpty, !code.isEmpty else { status.text = "Enter the instance address and pairing key."; return }
        connect.isEnabled = false
        connect.configuration?.showsActivityIndicator = true
        status.text = nil
        // Pairing changes the application's root; finish only if this screen is
        // still active. Cancellation must not sign in after the user dismisses.
        task = Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.task = nil; self.connect.isEnabled = true; self.connect.configuration?.showsActivityIndicator = false }
            do {
                try await self.app.connectInstance(endpoint: endpoint, key: code)
                if !Task.isCancelled { self.key.text = nil; self.dismiss(animated: true) }
            } catch {
                if !Task.isCancelled { self.status.text = "Could not connect: \(error)" }
            }
        }
    }
}
