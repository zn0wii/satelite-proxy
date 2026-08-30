import type { CSSProperties, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";

interface Option {
  value: string;
  /** ReactNode — embed e.g. a status dot next to the label. */
  label: ReactNode;
}

interface Props {
  value: string;
  options: Option[];
  onChange: (value: string) => void;
  ariaLabel?: string;
  disabled?: boolean;
  /** Per-option disable (e.g. gating TUN when no nodes). */
  disabledValues?: Set<string>;
  /** Per-option title tooltip. */
  titles?: Record<string, string>;
  /** False while the parent is still loading the initial persisted value. */
  ready?: boolean;
}

/**
 * Three-way glass segmented control. The active option is marked by a sliding
 * frosted-glass capsule (same material as the navbar) that travels between
 * positions; re-used across the dashboard quick controls.
 */
export function GlassSeg({
  value,
  options,
  onChange,
  ariaLabel,
  disabled,
  disabledValues,
  titles,
  ready = true,
}: Props) {
  const index = Math.max(
    0,
    options.findIndex((o) => o.value === value),
  );

  // A GlassSeg is controlled, so its value can change for two very different
  // reasons: the user clicked this control, or its parent loaded/refreshed
  // state. Only the former should slide. Persisted state, polling updates and
  // option-list changes must paint their target position directly.
  const committedValueRef = useRef(value);
  const committedIndexRef = useRef(index);
  const pendingUserValueRef = useRef<string | null>(null);
  const positionChanged =
    committedValueRef.current !== value || committedIndexRef.current !== index;
  const isUserChange = pendingUserValueRef.current === value;

  // Suppress transitions through the first paint of the persisted value — it
  // can arrive well after mount. Otherwise returning to a page makes the
  // capsule slide from the fallback option to the actual saved option.
  const [canAnimate, setCanAnimate] = useState(false);
  useEffect(() => {
    if (!ready) {
      setCanAnimate(false);
      return;
    }

    // Two frames guarantee that the no-transition target state has actually
    // painted before transitions are enabled for later user changes.
    let nextRaf = 0;
    const paintRaf = requestAnimationFrame(() => {
      nextRaf = requestAnimationFrame(() => setCanAnimate(true));
    });
    return () => {
      cancelAnimationFrame(paintRaf);
      cancelAnimationFrame(nextRaf);
    };
  }, [ready]);

  useEffect(() => {
    committedValueRef.current = value;
    committedIndexRef.current = index;
    if (pendingUserValueRef.current === value) {
      pendingUserValueRef.current = null;
    }
  }, [index, value]);

  const animateIndicator =
    canAnimate && (!positionChanged || isUserChange);

  return (
    <div
      className="glass-seg"
      role="group"
      aria-label={ariaLabel}
      style={{ "--count": options.length } as CSSProperties}
    >
      <span
        className={`glass-seg-indicator${animateIndicator ? "" : " no-anim"}`}
        aria-hidden="true"
        style={{ transform: `translateX(${index * 100}%)` }}
      />
      {options.map((o) => {
        const isDisabled = disabled || disabledValues?.has(o.value);
        return (
          <button
            key={o.value}
            type="button"
            className={`glass-seg-btn ${value === o.value ? "active" : ""}`}
            disabled={isDisabled}
            title={titles?.[o.value]}
            onClick={() => {
              pendingUserValueRef.current = o.value;
              onChange(o.value);
            }}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
