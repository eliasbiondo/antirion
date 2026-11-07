//! Language of the human-readable messages sent to a console user.

use serde_json::Value;

use crate::Runtime;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConsoleLocale {
    #[default]
    English,
    Persian,
}

impl ConsoleLocale {
    #[must_use]
    pub fn from_preferences(preferences: &Value) -> Self {
        let language = preferences
            .get("locale")
            .and_then(Value::as_str)
            .and_then(|tag| tag.split('-').next());
        if language.is_some_and(|tag| tag.eq_ignore_ascii_case("fa")) {
            Self::Persian
        } else {
            Self::English
        }
    }

    #[must_use]
    pub const fn tag(self) -> &'static str {
        self.pick("en", "fa")
    }

    #[must_use]
    pub const fn pick<'a>(self, english: &'a str, persian: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Persian => persian,
        }
    }

    pub(crate) fn severity(self, value: &str) -> &str {
        self.pick(
            value,
            match value {
                "critical" => "بحرانی",
                "warning" => "هشدار",
                "info" => "اطلاعاتی",
                _ => value,
            },
        )
    }

    pub(crate) fn category(self, value: &str) -> &str {
        self.pick(
            value,
            match value {
                "availability" => "دسترس‌پذیری",
                "latency" => "تأخیر",
                "budget" => "بودجه",
                "safety" => "ایمنی",
                "capacity" => "ظرفیت",
                "cache" => "کش",
                "anomaly" => "فعالیت غیرعادی",
                _ => value,
            },
        )
    }
}

impl Runtime {
    /// A user without an explicit saved language receives English messages.
    pub async fn console_locale(&self, user: &str) -> ConsoleLocale {
        self.preferences(user)
            .await
            .as_ref()
            .map_or(ConsoleLocale::default(), ConsoleLocale::from_preferences)
    }
}
