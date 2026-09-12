/**
 * Global replacement of native `title` tooltips.
 *
 * Every `title` attribute is migrated to `data-fast-tip` the moment it
 * appears (MutationObserver + initial pass), so no `title` ever stays in the
 * DOM — the native popup cannot fire at all. (Merely blanking the title on
 * mouseover raced the WebView's own tooltip scheduling and intermittently
 * produced BOTH popups.) Hovering a `[data-fast-tip]` element shows a styled
 * bubble after a short delay; an empty value marks an opt-out that also
 * masks any ancestor's tooltip (empty-title pattern, see node cards).
 *
 * Multiline values (the node hover card uses `\n`) render via
 * `white-space: pre-line`. Pure DOM — install once at startup (main.tsx).
 */

const TIP_ID = "satelite-fast-tip";
const TIP_ATTR = "data-fast-tip";
/** Elements tagged `data-tip-kind="node"` (the shared node hover card) show
 *  fast; every other tooltip waits longer so hints don't feel twitchy. */
const NODE_KIND_ATTR = "data-tip-kind";
const NODE_KIND = "node";

export function installFastTitleTooltips(nodeDelayMs = 120, otherDelayMs = 400) {
  if (document.getElementById(TIP_ID)) return;

  const tip = document.createElement("div");
  tip.id = TIP_ID;
  Object.assign(tip.style, {
    position: "fixed",
    // Fixed coordinates at all times — a frame with auto coordinates would
    // drop the box to its static position at the end of the document,
    // flashing the page taller and making the content jump.
    top: "0",
    left: "0",
    zIndex: "9999",
    pointerEvents: "none",
    background: "var(--panel-solid)",
    color: "var(--text)",
    border: "1px solid var(--border)",
    borderRadius: "6px",
    padding: "6px 10px",
    fontSize: "0.78rem",
    lineHeight: "1.55",
    whiteSpace: "pre-line",
    maxWidth: "26rem",
    wordBreak: "break-word",
    boxShadow: "0 6px 20px rgba(0, 0, 0, 0.35)",
    opacity: "0",
    visibility: "hidden",
    transition: "opacity 0.12s ease",
  });
  document.body.appendChild(tip);

  let showTimer = 0;
  let currentEl: HTMLElement | null = null;
  let lastX = 0;
  let lastY = 0;

  const hide = () => {
    window.clearTimeout(showTimer);
    tip.style.visibility = "hidden";
    tip.style.opacity = "0";
    currentEl = null;
  };

  const place = () => {
    const w = tip.offsetWidth;
    const h = tip.offsetHeight;
    let x = lastX + 14;
    let y = lastY - h - 12;
    if (y < 6) y = lastY + 18;
    if (x + w > window.innerWidth - 6) x = Math.max(6, lastX - w - 14);
    tip.style.left = `${x}px`;
    tip.style.top = `${y}px`;
  };

  const show = (text: string) => {
    tip.textContent = text;
    place();
    tip.style.visibility = "visible";
    tip.style.opacity = "1";
  };

  // —— title migration: no title attribute survives in the DOM ———
  const migrateEl = (el: Element) => {
    // Guard: our own removal re-enters the observer; without this check the
    // callback would see "no title" and wipe the just-migrated value.
    if (!el.hasAttribute("title")) return;
    const value = el.getAttribute("title");
    // Empty value keeps an empty marker: hovering it neither shows a tooltip
    // nor falls through to an ancestor's (the empty-title opt-out pattern).
    el.setAttribute(TIP_ATTR, value ?? "");
    el.removeAttribute("title");
  };
  const migrateTree = (root: Element) => {
    if (root.matches("[title]")) migrateEl(root);
    root.querySelectorAll("[title]").forEach(migrateEl);
  };
  migrateTree(document.documentElement);
  new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "attributes") {
        migrateEl(record.target as Element);
      } else {
        record.addedNodes.forEach((node) => {
          if (node.nodeType === Node.ELEMENT_NODE) {
            migrateTree(node as Element);
          }
        });
      }
    }
  }).observe(document.documentElement, {
    subtree: true,
    childList: true,
    attributeFilter: ["title"],
  });

  document.addEventListener(
    "mouseover",
    (e) => {
      const target = e.target as HTMLElement | null;
      const el = target?.closest?.(`[${TIP_ATTR}]`) as HTMLElement | null;
      const text = el?.getAttribute(TIP_ATTR) ?? "";
      if (!el || !text.trim()) {
        if (currentEl) hide();
        return;
      }
      lastX = e.clientX;
      lastY = e.clientY;
      // Hopping between stacked elements that carry the SAME text (node card
      // → its name row) must not flicker: just rebind.
      if (currentEl && currentEl.getAttribute(TIP_ATTR) === text) {
        currentEl = el;
        return;
      }
      hide();
      currentEl = el;
      const delay =
        el.getAttribute(NODE_KIND_ATTR) === NODE_KIND ? nodeDelayMs : otherDelayMs;
      showTimer = window.setTimeout(() => show(text), delay);
    },
    true,
  );

  document.addEventListener(
    "mousemove",
    (e) => {
      lastX = e.clientX;
      lastY = e.clientY;
    },
    true,
  );

  document.addEventListener(
    "mouseout",
    (e) => {
      if (!currentEl) return;
      const to = e.relatedTarget as Node | null;
      if (to && currentEl.contains(to)) return;
      hide();
    },
    true,
  );

  // Any scroll (including inner virtualized lists) or interaction dismisses.
  window.addEventListener("scroll", hide, { capture: true, passive: true });
  document.addEventListener("mousedown", hide, true);
  window.addEventListener("blur", hide);
}
