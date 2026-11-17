// SOLO: element picking for Solo's browser. Injected while "Pick" is on and
// removed when it's off: it leaves nothing behind in the page.
(() => {
  if (window.__soloPick) {
    window.__soloPick.start();
    return;
  }
  const post = (message) => {
    try {
      window.chrome.webview.postMessage(JSON.stringify(message));
    } catch (_) {}
  };

  let host = null;
  let box = null;
  let label = null;
  let badgeLayer = null;
  let current = null;
  let marks = [];
  let active = false;
  let frame = 0;

  const shortName = (element) => {
    let name = element.tagName.toLowerCase();
    if (element.id) return name + "#" + element.id;
    const classes = Array.from(element.classList).slice(0, 2);
    if (classes.length) name += "." + classes.join(".");
    return name;
  };

  const isOurs = (node) => host && (node === host || host.contains(node));

  const snapshot = () => {
    let budget = 20000;
    const visit = (element) => {
      budget -= 1;
      const node = { t: element.tagName.toLowerCase() };
      if (element.id) node.i = element.id;
      const testId = element.getAttribute("data-testid");
      if (testId) node.d = testId;
      if (element.classList.length) node.c = Array.from(element.classList);
      const children = [];
      for (const child of element.children) {
        if (isOurs(child)) continue;
        if (budget <= 0) {
          children.push({ t: child.tagName.toLowerCase() });
          continue;
        }
        children.push(visit(child));
      }
      if (children.length) node.k = children;
      return node;
    };
    return visit(document.documentElement);
  };

  const pathOf = (element) => {
    const path = [];
    while (element && element !== document.documentElement) {
      const parent = element.parentElement;
      if (!parent) break;
      path.unshift(Array.prototype.indexOf.call(parent.children, element));
      element = parent;
    }
    return path;
  };

  const hex = (color) => {
    const match = /rgba?\(([^)]+)\)/.exec(color);
    if (!match) return color;
    const parts = match[1].split(/[ ,/]+/).filter(Boolean).map(Number);
    if (parts.length === 4 && parts[3] === 0) return "transparent";
    const value = parts
      .slice(0, 3)
      .map((part) => Math.round(part).toString(16).padStart(2, "0"))
      .join("");
    return "#" + value;
  };

  const stylesOf = (element) => {
    const style = getComputedStyle(element);
    const styles = [];
    const add = (name, value) => {
      if (value !== undefined && value !== null && value !== "") styles.push([name, value]);
    };
    add("color", hex(style.color));
    const background = hex(style.backgroundColor);
    if (background !== "transparent") add("background", background);
    add("font-family", style.fontFamily.split(",")[0].trim().replace(/["']/g, ""));
    add("font-size", style.fontSize);
    if (style.fontWeight !== "400") add("font-weight", style.fontWeight);
    const box = (prefix) => {
      const sides = ["Top", "Right", "Bottom", "Left"].map((side) => style[prefix + side]);
      if (sides.every((side) => side === "0px")) return null;
      if (sides[0] === sides[2] && sides[1] === sides[3]) {
        return sides[0] === sides[1] ? sides[0] : sides[0] + " " + sides[1];
      }
      return sides.join(" ");
    };
    const padding = box("padding");
    if (padding) add("padding", padding);
    const margin = box("margin");
    if (margin) add("margin", margin);
    const rect = element.getBoundingClientRect();
    add("width", Math.round(rect.width) + "px");
    add("height", Math.round(rect.height) + "px");
    return styles;
  };

  const ensureHost = () => {
    if (host) return;
    host = document.createElement("solo-pick");
    host.style.cssText = "all:initial;position:fixed;inset:0;pointer-events:none;z-index:2147483647";
    const shadow = host.attachShadow({ mode: "closed" });
    shadow.innerHTML =
      '<style>' +
      '.box{position:fixed;outline:2px solid #59C2FF;outline-offset:2px;background:rgba(89,194,255,.12);pointer-events:none;display:none}' +
      '.label{position:fixed;font:10.5px/1.4 Consolas,monospace;background:#0B0E14;color:#8FD6FF;padding:2px 6px;border-radius:3px;white-space:nowrap;pointer-events:none;display:none}' +
      '.badge{position:fixed;width:17px;height:17px;border-radius:50%;background:#59C2FF;color:#0B0E14;font:700 10.5px/17px system-ui,sans-serif;text-align:center;pointer-events:auto;cursor:pointer}' +
      '.mark{position:fixed;outline:2px solid #59C2FF;outline-offset:2px;pointer-events:none}' +
      '</style><div class="box"></div><div class="label"></div><div class="badges"></div>';
    box = shadow.querySelector(".box");
    label = shadow.querySelector(".label");
    badgeLayer = shadow.querySelector(".badges");
    document.documentElement.appendChild(host);
  };

  const place = (element) => {
    current = element;
    if (!element) {
      box.style.display = "none";
      label.style.display = "none";
      return;
    }
    const rect = element.getBoundingClientRect();
    Object.assign(box.style, {
      display: "block",
      left: rect.left + "px",
      top: rect.top + "px",
      width: rect.width + "px",
      height: rect.height + "px",
    });
    label.textContent =
      shortName(element) + "  " + Math.round(rect.width) + " × " + Math.round(rect.height);
    label.style.display = "block";
    label.style.left = Math.max(0, rect.left) + "px";
    label.style.top = (rect.top > 22 ? rect.top - 22 : rect.bottom + 4) + "px";
  };

  const drawMarks = () => {
    if (!badgeLayer) return;
    badgeLayer.textContent = "";
    for (const mark of marks) {
      let element = null;
      try {
        element = document.querySelector(mark.selector);
      } catch (_) {}
      if (!element) continue;
      const rect = element.getBoundingClientRect();
      const outline = document.createElement("div");
      outline.className = "mark";
      Object.assign(outline.style, {
        left: rect.left + "px",
        top: rect.top + "px",
        width: rect.width + "px",
        height: rect.height + "px",
      });
      const badge = document.createElement("div");
      badge.className = "badge";
      badge.textContent = String(mark.n);
      badge.style.left = Math.max(0, rect.right - 8) + "px";
      badge.style.top = Math.max(0, rect.top - 9) + "px";
      badge.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        const box = element.getBoundingClientRect();
        post({
          type: "mark",
          n: mark.n,
          rect: { x: box.left, y: box.top, width: box.width, height: box.height },
        });
      });
      badgeLayer.appendChild(outline);
      badgeLayer.appendChild(badge);
    }
  };

  const tick = () => {
    if (!active) return;
    drawMarks();
    if (current && current.isConnected) place(current);
    frame = requestAnimationFrame(tick);
  };

  const target = (event) => {
    const element = document.elementFromPoint(event.clientX, event.clientY);
    return element && !isOurs(element) ? element : null;
  };

  const swallow = (event) => {
    if (event.composedPath && host && event.composedPath().includes(host)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  };

  const onMove = (event) => {
    const element = target(event);
    if (element !== current) place(element);
  };

  const onClick = (event) => {
    if (event.composedPath && host && event.composedPath().includes(host)) return;
    swallow(event);
    const element = target(event);
    if (!element) return;
    const rect = element.getBoundingClientRect();
    post({
      type: "pick",
      path: pathOf(element),
      text: (element.innerText || element.textContent || "").slice(0, 200),
      html: element.outerHTML.slice(0, 4096),
      styles: stylesOf(element),
      rect: { x: rect.left, y: rect.top, width: rect.width, height: rect.height },
      scrollX: window.scrollX,
      scrollY: window.scrollY,
      document: snapshot(),
    });
  };

  const onKey = (event) => {
    if (event.key === "Escape") {
      swallow(event);
      post({ type: "cancel" });
    }
  };

  const events = ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "auxclick", "contextmenu"];

  window.__soloPick = {
    start() {
      ensureHost();
      if (active) return;
      active = true;
      window.addEventListener("mousemove", onMove, true);
      window.addEventListener("click", onClick, true);
      window.addEventListener("keydown", onKey, true);
      for (const name of events) window.addEventListener(name, swallow, true);
      frame = requestAnimationFrame(tick);
    },
    stop() {
      active = false;
      cancelAnimationFrame(frame);
      window.removeEventListener("mousemove", onMove, true);
      window.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onKey, true);
      for (const name of events) window.removeEventListener(name, swallow, true);
      if (host) host.remove();
      host = box = label = badgeLayer = current = null;
      delete window.__soloPick;
    },
    setMarks(list) {
      marks = list;
      drawMarks();
    },
    found(selectors) {
      return selectors.map((selector) => {
        try {
          return !!document.querySelector(selector);
        } catch (_) {
          return false;
        }
      });
    },
  };
  window.__soloPick.start();
})();
