/**
 * Extend's mark (public/brand/mark.svg): Interface's ring of squares, with its north-east square
 * stepped one gutter out, reaching a square beyond it (another device). Drawn in `currentColor`, so
 * it takes the ink of whatever holds it, like Interface's mark in its rail.
 */
const MARK =
  "M0 22h9v9h-9zM4 13h9v9h-9zM13 9h9v9h-9zM26 22h9v9h-9zM22 31h9v9h-9zM13 35h9v9h-9zM4 31h9v9h-9z" +
  "M17.5 16.79L27.21 26.5L17.5 36.21L7.79 26.5z" +
  "M26 9h9v9h-9zM35 0h9v9h-9z";

export function ExtendMark(props: { size?: number }) {
  const size = () => props.size ?? 28;
  return (
    <svg class="extend-mark" width={size()} height={size()} viewBox="0 0 44 44" fill="currentColor" aria-hidden="true">
      <path d={MARK} />
    </svg>
  );
}
