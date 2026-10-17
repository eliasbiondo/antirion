//! `admin.ui`: where the console is mounted on the admin listener.

/// `admin.ui`: the console on the admin listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Console {
    pub enabled: bool,
    /// The mount, without a trailing slash (`/ui`; empty for the root).
    pub path: String,
    /// `admin.ui.data_plane_url`, reported by the status endpoint.
    pub data_plane_url: Option<String>,
}

impl Default for Console {
    fn default() -> Console {
        Console {
            enabled: true,
            path: "/ui".to_owned(),
            data_plane_url: None,
        }
    }
}

impl Console {
    /// The part of `path` under the mount, without its leading slash (`""` for the mount itself),
    /// when the console is enabled and `path` is under it.
    #[must_use]
    pub fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        if !self.enabled {
            return None;
        }
        let rest = path.strip_prefix(self.path.as_str())?;
        if rest.is_empty() {
            return Some("");
        }
        rest.strip_prefix('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str) -> Console {
        Console {
            path: path.to_owned(),
            ..Console::default()
        }
    }

    #[test]
    fn paths_under_the_mount() {
        let c = Console::default();
        assert_eq!(c.relative("/ui"), Some(""));
        assert_eq!(c.relative("/ui/"), Some(""));
        assert_eq!(c.relative("/ui/keys/"), Some("keys/"));
        assert_eq!(c.relative("/ui/_next/static/a.js"), Some("_next/static/a.js"));
        assert_eq!(c.relative("/uix"), None);
        assert_eq!(c.relative("/admin/v1/status"), None);
        assert_eq!(at("").relative("/"), Some(""));
        assert_eq!(at("").relative("/keys"), Some("keys"));
        assert_eq!(at("/ops/console").relative("/ops/console/x"), Some("x"));
    }

    #[test]
    fn a_disabled_console_answers_nothing() {
        let c = Console {
            enabled: false,
            ..Console::default()
        };
        assert_eq!(c.relative("/ui/"), None);
    }
}
