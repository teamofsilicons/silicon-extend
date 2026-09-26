import { plural } from "../lib/format";

/** How long a device stays paired without activity: 1 to 30 days, 14 by default. */
export function TtlSlider(props: { value: number; onInput: (days: number) => void; id?: string; disabled?: boolean }) {
  const id = () => props.id ?? "ttl";
  return (
    <div class="ttl">
      <label for={id()}>
        Stays paired for <strong data-testid="ttl-value">{plural(props.value, "day")}</strong> without activity
      </label>
      <input
        id={id()}
        type="range"
        min="1"
        max="30"
        step="1"
        value={props.value}
        disabled={props.disabled}
        data-testid="ttl-slider"
        aria-valuetext={plural(props.value, "day")}
        onInput={(e) => props.onInput(Number(e.currentTarget.value))}
      />
      <div class="ttl-scale" aria-hidden="true">
        <span>1 day</span>
        <span>14</span>
        <span>30 days</span>
      </div>
      <p class="fine">
        A Silicon using the device, or you changing its settings, counts as activity. When the time runs out the device unpairs itself and every Silicon loses access.
      </p>
    </div>
  );
}
