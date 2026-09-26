/** A small history router: the site has a handful of routes and needs nothing more. */
import { createSignal, type JSX, splitProps } from "solid-js";

const current = () => (typeof location === "undefined" ? { pathname: "/", search: "" } : { pathname: location.pathname, search: location.search });
const [loc, setLoc] = createSignal(current());

if (typeof window !== "undefined") window.addEventListener("popstate", () => setLoc(current()));

export function useLocation() {
  return loc;
}

export function navigate(to: string, options: { replace?: boolean } = {}) {
  if (options.replace) history.replaceState(null, "", to);
  else history.pushState(null, "", to);
  setLoc(current());
  if (!options.replace && !to.includes("#")) window.scrollTo({ top: 0 });
}

export function query(): URLSearchParams {
  return new URLSearchParams(loc().search);
}

/** Matches `/devices/:id` style patterns; returns params or null. */
export function match(pattern: string, pathname: string): Record<string, string> | null {
  const p = pattern.split("/").filter(Boolean);
  const s = pathname.split("/").filter(Boolean);
  if (p.length !== s.length) return null;
  const params: Record<string, string> = {};
  for (let i = 0; i < p.length; i++) {
    if (p[i].startsWith(":")) params[p[i].slice(1)] = decodeURIComponent(s[i]);
    else if (p[i] !== s[i]) return null;
  }
  return params;
}

export function Link(props: JSX.AnchorHTMLAttributes<HTMLAnchorElement> & { href: string }) {
  const [local, rest] = splitProps(props, ["href", "onClick"]);
  return (
    <a
      {...rest}
      href={local.href}
      onClick={(event) => {
        if (typeof local.onClick === "function") (local.onClick as (e: MouseEvent) => void)(event);
        if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
        if (rest.target === "_blank" || /^[a-z]+:/i.test(local.href)) return;
        event.preventDefault();
        navigate(local.href);
      }}
    />
  );
}
