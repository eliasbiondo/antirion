// Applies the stored display preferences before the first paint, so a dark-theme user never sees
// a light flash. It reads the same localStorage key as the preferences store.
(function () {
  var root = document.documentElement;
  var prefs = {};
  var version = 0;
  try {
    var raw = localStorage.getItem('antirion.console.preferences');
    var stored = raw ? JSON.parse(raw) || {} : {};
    prefs = stored.state || {};
    version = stored.version || 0;
  } catch (e) {
    prefs = {};
  }
  // Dark unless chosen otherwise; version 0 stored the old default, `system`, as if chosen.
  var theme = prefs.theme || 'dark';
  if (theme === 'system' && version < 1) theme = 'dark';
  if (theme === 'system') {
    theme =
      window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  root.setAttribute('data-theme', theme);
  root.setAttribute('data-density', prefs.density || 'compact');
  root.setAttribute('data-sidebar', prefs.sidebarCollapsed ? 'collapsed' : 'expanded');
  if (prefs.reducedMotion) root.setAttribute('data-motion', 'reduced');
  if (typeof prefs.accentHue === 'number') root.style.setProperty('--ant-accent-h', String(prefs.accentHue));
})();
