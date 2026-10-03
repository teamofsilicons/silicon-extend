import { For } from "solid-js";

/** Solid port of UIArc's free segmented control; see web/UIARC.md and its MIT notice. */
export function SegmentedTabs<T extends string>(props: {
  label: string;
  value: T;
  change: (value: T) => void;
  items: { value: T; label: string; testid?: string; title?: string }[];
}) {
  function navigate(event: KeyboardEvent) {
    const current = props.items.findIndex((item) => item.value === props.value);
    let index: number;
    switch (event.key) {
      case "ArrowRight": case "ArrowDown": index = (current + 1) % props.items.length; break;
      case "ArrowLeft": case "ArrowUp": index = (current - 1 + props.items.length) % props.items.length; break;
      case "Home": index = 0; break;
      case "End": index = props.items.length - 1; break;
      default: return;
    }
    event.preventDefault();
    props.change(props.items[index].value);
    (event.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>('[role="tab"]')[index]?.focus();
  }
  return (
    <div class="filter-chips" role="tablist" aria-label={props.label} onKeyDown={navigate}>
      <For each={props.items}>{(item) => (
        <button type="button" role="tab" aria-selected={props.value === item.value}
          tabIndex={props.value === item.value ? 0 : -1}
          class={props.value === item.value ? "selected" : ""}
          onClick={() => props.change(item.value)} title={item.title} data-testid={item.testid}>
          {item.label}
        </button>
      )}</For>
    </div>
  );
}
