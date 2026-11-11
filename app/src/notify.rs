//! Desktop / OS notifications.
//! Shows native notifications (e.g. Windows toast notifications) on important events
//! like Quick App completion, AI finishing its response/asking for input, or starting all services.

use notify_rust::Notification;

/// Send an OS notification in the background if notifications are enabled.
pub fn send(summary: &str, body: &str) {
    if !crate::settings::current().notifications {
        return;
    }
    let summary = summary.to_string();
    let body = body.to_string();

    std::thread::spawn(move || {
        let mut notif = Notification::new();
        notif.appname("myCraft")
            .summary(&summary)
            .body(&body);

        let root = crate::mycraft_root();
        let icon_path = root.join("assets").join("logo.png");
        if icon_path.is_file() {
            notif.image_path(&icon_path.to_string_lossy());
        }

        if let Err(e) = notif.show() {
            eprintln!("Failed to send desktop notification: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Run manually with cargo test -- --ignored test_send_notification
    fn test_send_notification() {
        let res = Notification::new()
            .appname("myCraft")
            .summary("myCraft Test")
            .body("This is a test notification from myCraft!")
            .show();
        assert!(res.is_ok(), "Notification error: {:?}", res.err());
    }
}
