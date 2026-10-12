/**
 * Silicon Extend's logo, as the Carbon drew it. `LOGO` is the whole icon, the mark in #181818 on a cream (#EDE8E0) square,
 * as a 200 by 200 SVG document: the browser and home-screen icons, the Open Graph image and the Silicon Apps listing use
 * it (lib/brand-svg.ts). `LOGO_MARK` is the mark alone, drawn in the text colour (currentColor), as the header shows it
 * beside the name (components/foundation/brand/app-mark.tsx).
 */
export const LOGO = `<svg width="200" height="200" viewBox="0 0 200 200" fill="none" xmlns="http://www.w3.org/2000/svg"><rect width="200" height="200" fill="#EDE8E0"/><path transform="translate(45 45) scale(2.5)" fill="#181818" d="M0 22h9v9h-9zM4 13h9v9h-9zM13 9h9v9h-9zM26 22h9v9h-9zM22 31h9v9h-9zM13 35h9v9h-9zM4 31h9v9h-9zM17.5 16.79L27.21 26.5L17.5 36.21L7.79 26.5zM26 9h9v9h-9zM35 0h9v9h-9z"/></svg>`;

export const LOGO_MARK = {
  viewBox: "0 0 44 44",
  body: `<path d="M0 22h9v9h-9zM4 13h9v9h-9zM13 9h9v9h-9zM26 22h9v9h-9zM22 31h9v9h-9zM13 35h9v9h-9zM4 31h9v9h-9zM17.5 16.79L27.21 26.5L17.5 36.21L7.79 26.5zM26 9h9v9h-9zM35 0h9v9h-9z" fill="currentColor"/>`,
};
