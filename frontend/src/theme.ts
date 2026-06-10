// Theme: "light" = dark chrome + light script panel (the original look);
// "dark" = everything dark. Persisted; applied via <html data-theme>.

export type Theme = "light" | "dark";
const KEY = "roughcut-theme";

export function currentTheme(): Theme {
  return (localStorage.getItem(KEY) as Theme) || "dark";
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem(KEY, theme);
  // Canvas surfaces (timeline) repaint from the active palette.
  window.dispatchEvent(new Event("rc-theme-changed"));
}

export function toggleTheme(): Theme {
  const next: Theme = currentTheme() === "dark" ? "light" : "dark";
  applyTheme(next);
  return next;
}

// Apply on module load (imported from main.tsx before first render).
applyTheme(currentTheme());
