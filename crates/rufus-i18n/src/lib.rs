//! Lightweight localization. Untranslated keys fall back to English.
//!
//! Full parity with upstream's 38 languages is progressive: catalogs can be
//! loaded from Fluent/gettext files under `assets/i18n/` in packaging.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Catalog {
    locale: String,
    strings: HashMap<&'static str, &'static str>,
}

impl Catalog {
    pub fn locale(&self) -> &str {
        &self.locale
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).copied().unwrap_or(key)
    }
}

pub fn available_locales() -> &'static [(&'static str, &'static str)] {
    &[
        ("en", "English"),
        ("de", "Deutsch"),
        ("es", "Español"),
        ("fr", "Français"),
        ("it", "Italiano"),
        ("nl", "Nederlands"),
        ("pl", "Polski"),
        ("pt_BR", "Português (Brasil)"),
        ("ru", "Русский"),
        ("zh_CN", "简体中文"),
        ("zh_TW", "正體中文"),
        ("ja", "日本語"),
        ("ko", "한국어"),
        ("ar", "العربية"),
        ("tr", "Türkçe"),
        ("uk", "Українська"),
        ("sv", "Svenska"),
        ("cs", "Čeština"),
        ("hu", "Magyar"),
        ("ro", "Română"),
        ("vi", "Tiếng Việt"),
        ("id", "Bahasa Indonesia"),
        ("th", "ไทย"),
        ("hi", "हिन्दी"),
        ("fi", "Suomi"),
        ("da", "Dansk"),
        ("nb", "Norsk bokmål"),
        ("el", "Ελληνικά"),
        ("he", "עברית"),
        ("fa", "فارسی"),
        ("bg", "Български"),
        ("hr", "Hrvatski"),
        ("sk", "Slovenčina"),
        ("sl", "Slovenščina"),
        ("lt", "Lietuvių"),
        ("lv", "Latviešu"),
        ("et", "Eesti"),
        ("sr", "Српски"),
    ]
}

pub fn load(locale: &str) -> Catalog {
    let base = english();
    let mut strings = base;
    // Overlay partial translations when present.
    if let Some(overlay) = overlay_for(locale) {
        for (k, v) in overlay {
            strings.insert(k, v);
        }
    }
    Catalog {
        locale: locale.to_owned(),
        strings,
    }
}

fn english() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("app.title", "Rufus Linux"),
        ("app.subtitle", "Device Workbench"),
        ("section.device", "Drive"),
        ("section.boot", "Boot selection"),
        ("section.image", "Image options"),
        ("section.format", "Format options"),
        ("section.status", "Status"),
        ("label.device", "Device"),
        ("label.boot_type", "Boot selection"),
        ("label.partition", "Partition scheme"),
        ("label.target", "Target system"),
        ("label.filesystem", "File system"),
        ("label.cluster", "Cluster size"),
        ("label.volume", "Volume label"),
        ("label.persistence", "Persistence"),
        ("action.start", "Start"),
        ("action.cancel", "Cancel"),
        ("action.close", "Close"),
        ("action.refresh", "Refresh"),
        ("action.select_iso", "SELECT"),
        ("action.checksums", "Checksums"),
        ("action.log", "Log"),
        ("action.about", "About"),
        ("action.settings", "Settings"),
        ("action.download", "Download"),
        ("boot.disk_or_iso", "Disk or ISO image"),
        ("boot.non_bootable", "Non bootable"),
        ("boot.freedos", "FreeDOS"),
        ("confirm.write", "Write image"),
        ("confirm.format", "Format device"),
        (
            "confirm.destroy",
            "This will DESTROY all data on the selected device.",
        ),
        ("status.ready", "Ready"),
        ("status.scanning", "Scanning devices…"),
        ("status.done", "Done"),
        ("status.failed", "Failed"),
        ("status.cancelled", "Cancelled"),
        ("option.quick_format", "Quick format"),
        ("option.check_bad_blocks", "Check device for bad blocks"),
        (
            "option.create_extended",
            "Create extended label and icon files",
        ),
        ("option.verify", "Verify once written"),
        ("advanced.show_usb_hdd", "List USB hard drives"),
        (
            "advanced.show_fixed",
            "List fixed/internal disks (dangerous)",
        ),
        (
            "about.blurb",
            "Independent Linux port inspired by Rufus. GPLv3.",
        ),
    ])
}

fn overlay_for(locale: &str) -> Option<HashMap<&'static str, &'static str>> {
    let lang = locale.split(['_', '-']).next().unwrap_or(locale);
    match lang {
        "de" => Some(HashMap::from([
            ("app.title", "Rufus Linux"),
            ("section.device", "Laufwerk"),
            ("section.boot", "Boot-Auswahl"),
            ("section.format", "Formatierungsoptionen"),
            ("action.start", "Start"),
            ("action.cancel", "Abbrechen"),
            ("action.close", "Schließen"),
            ("status.ready", "Bereit"),
            (
                "confirm.destroy",
                "Dadurch werden ALLE Daten auf dem gewählten Gerät ZERSTÖRT.",
            ),
        ])),
        "fr" => Some(HashMap::from([
            ("section.device", "Périphérique"),
            ("section.boot", "Type de démarrage"),
            ("section.format", "Options de formatage"),
            ("action.start", "Démarrer"),
            ("action.cancel", "Annuler"),
            ("action.close", "Fermer"),
            ("status.ready", "Prêt"),
            (
                "confirm.destroy",
                "Cela DÉTRUIRA toutes les données du périphérique sélectionné.",
            ),
        ])),
        "es" => Some(HashMap::from([
            ("section.device", "Dispositivo"),
            ("section.boot", "Tipo de arranque"),
            ("section.format", "Opciones de formato"),
            ("action.start", "Empezar"),
            ("action.cancel", "Cancelar"),
            ("action.close", "Cerrar"),
            ("status.ready", "Listo"),
            (
                "confirm.destroy",
                "Esto DESTRUIRÁ todos los datos del dispositivo seleccionado.",
            ),
        ])),
        "pt" | "pt_BR" => Some(HashMap::from([
            ("section.device", "Dispositivo"),
            ("action.start", "Iniciar"),
            ("action.cancel", "Cancelar"),
            ("status.ready", "Pronto"),
        ])),
        "zh" | "zh_CN" => Some(HashMap::from([
            ("section.device", "设备"),
            ("section.boot", "启动类型"),
            ("section.format", "格式化选项"),
            ("action.start", "开始"),
            ("action.cancel", "取消"),
            ("action.close", "关闭"),
            ("status.ready", "就绪"),
        ])),
        "ru" => Some(HashMap::from([
            ("section.device", "Устройство"),
            ("section.boot", "Тип загрузки"),
            ("section.format", "Параметры форматирования"),
            ("action.start", "Старт"),
            ("action.cancel", "Отмена"),
            ("action.close", "Закрыть"),
            ("status.ready", "Готово"),
        ])),
        "ar" => Some(HashMap::from([
            ("section.device", "الجهاز"),
            ("action.start", "ابدأ"),
            ("action.cancel", "إلغاء"),
            ("status.ready", "جاهز"),
        ])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_has_title() {
        let cat = load("en");
        assert_eq!(cat.get("app.title"), "Rufus Linux");
    }

    #[test]
    fn fallback_to_key() {
        let cat = load("en");
        assert_eq!(cat.get("missing.key"), "missing.key");
    }

    #[test]
    fn german_overlay() {
        let cat = load("de");
        assert_eq!(cat.get("action.cancel"), "Abbrechen");
        // Untranslated keys fall back to English base.
        assert_eq!(cat.get("app.subtitle"), "Device Workbench");
    }
}
