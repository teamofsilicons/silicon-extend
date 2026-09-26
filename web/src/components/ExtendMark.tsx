/** Extend's mark: two devices joined by an arch. */
export function ExtendMark(props: { size?: number }) {
  const size = () => props.size ?? 28;
  return (
    <svg class="extend-mark" width={size()} height={size()} viewBox="0 0 32 32" aria-hidden="true">
      <rect x="1" y="1" width="30" height="30" rx="8" fill="var(--blue)" />
      <path d="M7 21.5c2.2-7.2 15.8-7.2 18 0" fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round" />
      <rect x="5" y="20" width="5" height="6" rx="1.4" fill="#fff" />
      <rect x="22" y="20" width="5" height="6" rx="1.4" fill="#fff" />
      <circle cx="16" cy="11.8" r="1.8" fill="#fff" />
    </svg>
  );
}
