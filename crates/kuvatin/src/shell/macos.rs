//! Finder Quick Action ("Services") registration for macOS.
//!
//! Mirrors the Windows Explorer context-menu integration: for each entry in
//! [`super::ITEMS`] we write an Automator "Run Shell Script" Quick Action into
//! `~/Library/Services` that invokes this binary with the matching preset.
//! Quick Actions are plain `.workflow` bundles (an `Info.plist` declaring an
//! `NSServices` entry plus a `document.wflow` describing the action), so no code
//! signing or notarization is required for them to appear in Finder's
//! right-click *Quick Actions* section.

use super::ITEMS;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// `~/Library/Services`, where per-user Quick Actions live.
fn services_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library").join("Services"))
}

/// The `.workflow` bundle name for a menu label (drops the GUI ellipsis).
fn workflow_name(label: &str) -> String {
    let clean = label.trim_end_matches('…').trim();
    format!("Kuvatin - {clean}.workflow")
}

/// Minimal XML text escaping for embedding in a plist `<string>`.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The shell command the Quick Action runs. Input files arrive as `"$@"`
/// because the action is configured to pass input "as arguments".
fn command_string(exe: &str, preset: &str) -> String {
    if preset.is_empty() {
        format!("\"{exe}\" \"$@\"")
    } else {
        format!("\"{exe}\" --preset \"{preset}\" \"$@\"")
    }
}

/// `Contents/Info.plist` — declares the Service so Finder shows it on images.
fn info_plist(label: &str) -> String {
    let menu = xml_escape(&format!("Kuvatin: {}", label.trim_end_matches('…').trim()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSIconName</key>
			<string>NSActionTemplate</string>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>{menu}</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
				<string>public.image</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
"#
    )
}

/// `Contents/document.wflow` — an Automator workflow with a single
/// "Run Shell Script" action that receives the selected image files as
/// arguments and runs `command`.
fn document_wflow(command: &str) -> String {
    let cmd = xml_escape(command);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>521</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.path</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{cmd}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/zsh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.Automator.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>618E60C9-2D7B-44B0-9E50-3E0C4F0B0001</string>
				<key>Keywords</key>
				<array>
					<string>Shell</string>
					<string>Script</string>
					<string>Command</string>
					<string>Run</string>
					<string>Unix</string>
				</array>
				<key>OutputUUID</key>
				<string>618E60C9-2D7B-44B0-9E50-3E0C4F0B0002</string>
				<key>UUID</key>
				<string>618E60C9-2D7B-44B0-9E50-3E0C4F0B0003</string>
				<key>UnlocalizedApplications</key>
				<array>
					<string>Automator</string>
				</array>
				<key>arguments</key>
				<dict/>
				<key>isViewVisible</key>
				<integer>1</integer>
				<key>location</key>
				<string>309.000000:253.000000</string>
				<key>nibPath</key>
				<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/main.nib</string>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject.image</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<integer>0</integer>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#
    )
}

/// Re-scan the Services database so the new menu items appear without a logout.
fn refresh() {
    let _ = std::process::Command::new("/System/Library/CoreServices/pbs")
        .arg("-flush")
        .status();
}

pub fn register() -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let exe = exe.to_string_lossy().into_owned();
    let dir = services_dir()?;

    for (_, label, preset) in ITEMS {
        let contents = dir.join(workflow_name(label)).join("Contents");
        std::fs::create_dir_all(&contents)
            .with_context(|| format!("creating {}", contents.display()))?;
        std::fs::write(contents.join("Info.plist"), info_plist(label))
            .with_context(|| format!("writing Info.plist for {label}"))?;
        std::fs::write(
            contents.join("document.wflow"),
            document_wflow(&command_string(&exe, preset)),
        )
        .with_context(|| format!("writing document.wflow for {label}"))?;
    }

    refresh();
    println!("Kuvatin Finder Quick Actions registered.");
    Ok(())
}

pub fn unregister() -> Result<()> {
    let dir = services_dir()?;
    for (_, label, _) in ITEMS {
        let bundle = dir.join(workflow_name(label));
        if bundle.exists() {
            let _ = std::fs::remove_dir_all(&bundle);
        }
    }
    refresh();
    println!("Kuvatin Finder Quick Actions removed.");
    Ok(())
}
