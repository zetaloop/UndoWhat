use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    core::HSTRING,
};

pub struct Record {
    pub description: String,
    pub lines: Vec<String>,
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn show(record: Record) -> windows::core::Result<()> {
    let body = record.lines.join("\n");
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text hint-wrap=\"true\">{}</text></binding></visual></toast>",
        xml_escape(&record.description),
        xml_escape(&body),
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        "Microsoft.Windows.Explorer",
    ))?
    .Show(&toast)
}
