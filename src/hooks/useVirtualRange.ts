import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";

interface VirtualRangeOptions {
  itemCount: number;
  itemSize: number;
  itemsPerRow?: number;
  enabled?: boolean;
  overscanRows?: number;
  /** Scroll container to listen to. Defaults to the app shell scroller;
   * pages whose list scrolls inside its own panel pass e.g. ".logs-panel". */
  scrollerSelector?: string;
}

interface RowRange {
  startRow: number;
  endRow: number;
}

/** Window a fixed-height list against the app's existing scroll container. */
export function useVirtualRange({
  itemCount,
  itemSize,
  itemsPerRow = 1,
  enabled = true,
  overscanRows = 6,
  scrollerSelector = ".main",
}: VirtualRangeOptions) {
  const containerRef = useRef<HTMLElement | null>(null);
  const totalRows = Math.ceil(itemCount / itemsPerRow);
  const [rows, setRows] = useState<RowRange>(() => ({
    startRow: 0,
    endRow: Math.min(totalRows, 30),
  }));
  // Tracks whether `rows` reflects a real measurement of the current
  // scroll position rather than the initial guess above. Without this,
  // flipping `enabled` from false→true (e.g. a list growing past the
  // virtualize threshold while scrolled down) renders one frame windowed
  // to rows 0-30 before the layout effect below corrects it — collapsing
  // already-visible content and reading as a flash back to the top.
  const measuredRef = useRef(false);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!enabled || !container) {
      measuredRef.current = false;
      setRows({ startRow: 0, endRow: totalRows });
      return;
    }

    const scroller = container.closest<HTMLElement>(scrollerSelector);
    if (!scroller) {
      measuredRef.current = false;
      setRows({ startRow: 0, endRow: totalRows });
      return;
    }

    let frame = 0;
    const update = () => {
      frame = 0;
      const containerRect = container.getBoundingClientRect();
      const scrollerRect = scroller.getBoundingClientRect();
      const visibleTop = Math.max(0, scrollerRect.top - containerRect.top);
      const visibleBottom = Math.max(
        visibleTop,
        Math.min(containerRect.height, scrollerRect.bottom - containerRect.top),
      );
      const startRow = Math.max(
        0,
        Math.floor(visibleTop / itemSize) - overscanRows,
      );
      const endRow = Math.min(
        totalRows,
        Math.ceil(visibleBottom / itemSize) + overscanRows,
      );
      measuredRef.current = true;
      setRows((current) =>
        current.startRow === startRow && current.endRow === endRow
          ? current
          : { startRow, endRow },
      );
    };
    // Scroll-driven re-renders must land in the SAME frame as the scroll
    // offset change. React 18 otherwise schedules the state update on a
    // later task, so the browser paints one frame of "scrolled but not
    // re-windowed" content — rows then snap back into place a frame later,
    // which reads as jerky / accelerating scrolling (react-window flushes
    // synchronously in scroll handlers for the same reason). Equal-state
    // setRows bail out, so idle frames stay cheap. Only event callbacks
    // may flush: the layout-effect path runs inside React's lifecycle,
    // where flushSync is forbidden.
    const updateSync = () => {
      flushSync(update);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(updateSync);
    };

    update();
    scroller.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    const observer = new ResizeObserver(schedule);
    observer.observe(scroller);
    observer.observe(container);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      observer.disconnect();
      scroller.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, [enabled, itemSize, overscanRows, scrollerSelector, totalRows]);
  return useMemo(() => {
    if (!enabled || !measuredRef.current) {
      return {
        containerRef,
        start: 0,
        end: itemCount,
        paddingTop: 0,
        paddingBottom: 0,
      };
    }
    const startRow = Math.min(rows.startRow, totalRows);
    const endRow = Math.max(startRow, Math.min(rows.endRow, totalRows));
    return {
      containerRef,
      start: startRow * itemsPerRow,
      end: Math.min(itemCount, endRow * itemsPerRow),
      paddingTop: startRow * itemSize,
      paddingBottom: Math.max(0, (totalRows - endRow) * itemSize),
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- measuredRef is a ref, not reactive state
  }, [enabled, itemCount, itemSize, itemsPerRow, rows, totalRows]);
}
