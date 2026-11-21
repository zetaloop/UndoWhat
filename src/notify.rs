use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    core::HSTRING,
};

#[link(name = "runtimeobject")]
unsafe extern "system" {
    fn RoInitialize(init: u32) -> i32;
    fn RoUninitialize();
}

pub fn with_apartment(f: impl FnOnce()) {
    let initialized = unsafe { RoInitialize(1) } >= 0;
    f();
    if initialized {
        unsafe { RoUninitialize() };
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn show(title: &str, body: &str) -> windows::core::Result<()> {
    let body = if body.is_empty() {
        String::new()
    } else {
        format!("<text hint-wrap=\"true\">{}</text>", xml_escape(body))
    };
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text>{body}</binding></visual></toast>",
        xml_escape(title),
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        "Microsoft.Windows.Explorer",
    ))?
    .Show(&toast)
}
