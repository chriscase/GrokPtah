import AppKit

final class DemoDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow!
    private var normalField: NSTextField!
    private var priority: NSPopUpButton!
    private var status: NSTextField!

    func applicationDidFinishLaunching(_ notification: Notification) {
        let frame = NSRect(x: 0, y: 0, width: 720, height: 520)
        window = NSWindow(
            contentRect: frame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "GrokPtah Computer Use Demo"
        window.center()

        let content = NSView(frame: frame)
        content.translatesAutoresizingMaskIntoConstraints = false
        window.contentView = content

        let heading = NSTextField(labelWithString: "Semantic action fixture")
        heading.font = .systemFont(ofSize: 22, weight: .semibold)

        let normalLabel = NSTextField(labelWithString: "Project label")
        normalField = NSTextField(string: "public-demo-value")
        normalField.placeholderString = "Visible demo text"
        normalField.setAccessibilityLabel("Project label")

        let secureLabel = NSTextField(labelWithString: "Demo password")
        let secureField = NSSecureTextField(string: "fixture-secret-never-export")
        secureField.placeholderString = "Secure demo text"

        priority = NSPopUpButton()
        priority.addItems(withTitles: ["Low", "Normal", "High"])
        priority.selectItem(withTitle: "Normal")
        priority.setAccessibilityLabel("Priority")

        let action = NSButton(
            title: "Submit fixture",
            target: self,
            action: #selector(submitFixture)
        )
        action.bezelStyle = .rounded
        action.setAccessibilityLabel("Submit fixture")

        status = NSTextField(labelWithString: "Not submitted")
        status.setAccessibilityLabel("Fixture status")

        let longText = (1...40).map { "Accessible demo row \($0)" }.joined(separator: "\n")
        let textView = NSTextView()
        textView.string = longText
        textView.isEditable = false
        textView.isSelectable = true
        let scroll = NSScrollView()
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let stack = NSStackView(views: [
            heading, normalLabel, normalField, secureLabel, secureField, priority, action, status,
            scroll,
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(stack)

        normalField.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        secureField.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        scroll.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        scroll.heightAnchor.constraint(equalToConstant: 250).isActive = true
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -24),
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 24),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -24),
        ])

        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func submitFixture() {
        let label = normalField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let bounded = String(label.prefix(128))
        status.stringValue = "Submitted \(bounded) at \(priority.titleOfSelectedItem ?? "Normal")"
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

let app = NSApplication.shared
let delegate = DemoDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
