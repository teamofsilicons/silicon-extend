import { read, write } from "./storage";
import { KEYS } from "./session";

export type Theme = "auto" | "light" | "dark";

export function currentTheme(): Theme {
  const saved = read("local", KEYS.theme);
  return saved === "light" || saved === "dark" ? saved : "auto";
}

/** `auto` follows the system; light and dark pin it. Colours come from tokens in styles.css. */
export function applyTheme(theme: Theme = currentTheme()) {
  write("local", KEYS.theme, theme);
  if (theme === "auto") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", theme);
}
