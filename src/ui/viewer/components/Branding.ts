/**
 * "Powered by Anicca Code Studio" branding component.
 *
 * Hardened against host-page CSS injection that tries to suppress the mark:
 *
 *   - Visual content lives inside a closed shadow root, so external CSS
 *     (including `!important` rules) cannot select anything visible.
 *   - The light-DOM host wrapper carries a per-instance random class name,
 *     so attacker stylesheets cannot statically target it.
 *   - The literal anicca.dev URL is not rendered in any `href` attribute;
 *     a click handler decodes it from a base64 `data-h` attribute at runtime,
 *     defeating `a[href*="anicca.dev"]` selectors.
 *   - A MutationObserver re-inserts the host if it is removed from its parent.
 *   - For the persistent `viewport-corner` variant, a runtime visibility check
 *     (IntersectionObserver + ~2s setInterval inspection of getBoundingClientRect
 *     and getComputedStyle) trips a `adv:branding-suppressed` CustomEvent and
 *     invokes the caller's onSuppressed hook so a watermark fallback can engage.
 *
 * Defense-in-depth, not silver bullets: a determined attacker with arbitrary
 * JS access to the page can still tamper. The shadow root blocks CSS attacks;
 * the visibility tripwire converts any successful suppression into a signal
 * that downstream code (e.g. a rasterized watermark) can react to.
 */

// Capture `attachShadow` and `getComputedStyle` at module evaluation time so
// later prototype patching by the host page does not affect us. Guarded for
// non-DOM environments (SSR/Node) where the module may be imported but
// `createBranding` will never be invoked.
const __attachShadow: typeof Element.prototype.attachShadow =
    typeof Element !== "undefined" ? Element.prototype.attachShadow : (undefined as never);
const __getComputedStyle: (typeof window)["getComputedStyle"] =
    typeof window !== "undefined" ? window.getComputedStyle.bind(window) : (undefined as never);

// "https://anicca.dev" — held base64 so `a[href*="anicca.dev"]` and
// text-scans of the bundle do not find it on the rendered element.
const ATTRIBUTION_URL_B64 = "aHR0cHM6Ly9hbmljY2EuZGV2";

// SVG of the "Anicca Code Studio" wordmark; used by the loading and spread variants.
// Inside the closed shadow root, stable class names are fine.
const LOGO_SVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 170 24" width="142" height="20" aria-hidden="true"><text x="0" y="17" font-family="system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif" font-size="15"><tspan class="logo-name" font-weight="700">Anicca</tspan><tspan class="logo-accent" font-weight="500" dx="5">Code Studio</tspan></text></svg>`;

const LIGHT_NAME_FILL = "#0f172a";
const LIGHT_ACCENT_FILL = "#4f46e5";
const DARK_NAME_FILL = "#e2e8f0";
const DARK_ACCENT_FILL = "#818cf8";

export type BrandingVariant =
    /** Persistent corner badge in the viewport. Tripwire-monitored. */
    | "viewport-corner"
    /** Block shown under the loading-overlay progress bar. */
    | "loading-block"
    /** Logo-only mark for the in-page "rendering" indicator (positioning and
     *  slot-state visibility are handled by the caller). */
    | "spread-indicator";

export interface BrandingOptions {
    variant: BrandingVariant;
    /** Invoked the first time suppression is detected. Only fires for variants
     * that run a visibility tripwire. */
    onSuppressed?: (detail: { reason: string }) => void;
}

export interface BrandingHandle {
    /** Light-DOM host element to append where the branding should appear. */
    el: HTMLElement;
    /** Call after the host is in the document to start protection. */
    start(): void;
    /** Stop observers/intervals and remove the host. */
    destroy(): void;
}

const SUPPRESSED_EVENT = "adv:branding-suppressed";
const TRIPWIRE_INTERVAL_MS = 2000;
// Thresholds for "this is too small / faded to count as visible".
const MIN_VISIBLE_OPACITY = 0.05;
const MIN_VISIBLE_WIDTH_PX = 24;
const MIN_VISIBLE_HEIGHT_PX = 8;

export function createBranding(opts: BrandingOptions): BrandingHandle {
    // Per-instance random class — defeats static `.<known-name> { display: none }`
    // attacks against the light-DOM wrapper.
    const hostClass = "_b" + Math.random().toString(36).slice(2, 11);

    const host = document.createElement("div");
    host.className = hostClass;

    const shadow = __attachShadow.call(host, { mode: "closed" });
    const style = document.createElement("style");
    style.textContent = buildShadowCss(opts.variant);
    shadow.appendChild(style);

    // The visible link: a <button role="link"> rather than <a href> so that
    // `a[href*="anicca.dev"]` selectors find nothing even if the attacker
    // pierces the shadow boundary.
    const link = document.createElement("button");
    link.type = "button";
    link.className = "link";
    link.setAttribute("role", "link");
    link.setAttribute("aria-label", "Powered by Anicca Code Studio");
    link.setAttribute("part", "link");
    link.dataset.h = ATTRIBUTION_URL_B64;
    link.addEventListener("click", (e) => {
        e.preventDefault();
        e.stopPropagation();
        try {
            const url = atob(link.dataset.h ?? "");
            window.open(url, "_blank", "noopener,noreferrer");
        } catch {
            // ignore decode failures
        }
    });

    if (opts.variant === "viewport-corner") {
        // Compact "Powered by Anicca Code Studio" text badge.
        link.innerHTML = `Powered by <span class="logo-name">Anicca</span> <span class="logo-accent">Code Studio</span>`;
        shadow.appendChild(link);
    } else if (opts.variant === "loading-block") {
        link.innerHTML = LOGO_SVG;
        shadow.appendChild(link);
    } else {
        // spread-indicator: logo-only, non-interactive. The caller wraps this
        // alongside a "Rendering..." label in its own indicator container.
        link.innerHTML = LOGO_SVG;
        link.setAttribute("tabindex", "-1");
        shadow.appendChild(link);
    }

    // ---- Light-DOM hardening on the host ----

    // Inline !important styles win against external !important rules (CSS spec).
    // We re-apply these whenever the host's style attribute is touched.
    function applyHostInlineStyles(): void {
        const decls: string[] = [
            "visibility: visible !important",
            "opacity: 1 !important",
            "clip: auto !important",
            "clip-path: none !important",
            "filter: none !important",
            "pointer-events: auto !important",
            // Variant-specific layout. Crucially these include !important `display`
            // so that an external `display: none !important` cannot collapse us.
        ];
        if (opts.variant === "viewport-corner") {
            decls.push(
                "display: inline-flex !important",
                "position: absolute !important",
                "right: 18px !important",
                "bottom: 4px !important",
                "z-index: 10 !important",
            );
        } else if (opts.variant === "loading-block") {
            decls.push("display: inline-flex !important", "margin-top: 12px !important");
        } else {
            // spread-indicator: the host has no positioning of its own; the
            // caller's wrapper handles placement and visibility.
            decls.push("display: inline-flex !important");
        }
        host.style.cssText = decls.join(";");
    }
    applyHostInlineStyles();

    // ---- Theme sync ----
    // Custom properties inside the shadow read theme colors. We mirror the
    // viewer's `adv-viewer-dark` class onto a `data-dark` attribute on the
    // host so `:host([data-dark])` shadow rules can pick it up.
    let themeObserver: MutationObserver | null = null;
    function setupThemeSync(): void {
        const root = host.closest(".adv-viewer-root");
        if (!root) return;
        const apply = () => {
            if (root.classList.contains("adv-viewer-dark")) {
                host.setAttribute("data-dark", "");
            } else {
                host.removeAttribute("data-dark");
            }
        };
        apply();
        themeObserver = new MutationObserver(apply);
        themeObserver.observe(root, { attributes: true, attributeFilter: ["class"] });
    }

    // ---- Tamper restoration ----
    // Re-insert the host if the attacker (or our own re-render) removes it.
    // We deliberately do NOT observe attribute mutations on the host: doing so
    // creates a feedback loop because our own `style.cssText` writes count as
    // mutations. The closed shadow root blocks the CSS-injection attack we
    // care about, and the periodic tripwire check (viewport-corner only)
    // re-applies inline styles every 2s if anything has drifted.
    let parentRef: Node | null = null;
    let parentObserver: MutationObserver | null = null;
    function setupTamperProtection(): void {
        parentRef = host.parentNode;
        if (!parentRef) return;

        parentObserver = new MutationObserver(() => {
            if (parentRef && !parentRef.contains(host)) {
                parentRef.appendChild(host);
                applyHostInlineStyles();
            }
        });
        parentObserver.observe(parentRef, { childList: true });
    }

    // ---- Visibility tripwire ----
    let tripwireInterval: ReturnType<typeof setInterval> | null = null;
    let intersectionObserver: IntersectionObserver | null = null;
    let suppressed = false;

    function trigger(reason: string): void {
        if (suppressed) return;
        suppressed = true;
        try {
            host.dispatchEvent(
                new CustomEvent(SUPPRESSED_EVENT, {
                    bubbles: true,
                    composed: true,
                    detail: { reason, variant: opts.variant },
                }),
            );
        } catch {
            // ignore
        }
        try {
            opts.onSuppressed?.({ reason });
        } catch {
            // ignore handler errors
        }
    }

    function checkVisibility(): void {
        if (suppressed) return;
        if (!host.isConnected) {
            trigger("host-disconnected");
            return;
        }
        // Reapply hardening before measuring so we don't flag transient drift.
        if (host.className !== hostClass) host.className = hostClass;
        applyHostInlineStyles();

        const cs = __getComputedStyle(host);
        const rect = host.getBoundingClientRect();

        if (cs.display === "none") return trigger("display:none");
        if (cs.visibility === "hidden" || cs.visibility === "collapse") return trigger(`visibility:${cs.visibility}`);
        if (parseFloat(cs.opacity) < MIN_VISIBLE_OPACITY) return trigger(`opacity:${cs.opacity}`);
        if (cs.clip !== "auto" && cs.clip !== "") return trigger(`clip:${cs.clip}`);
        if (cs.clipPath && cs.clipPath !== "none") return trigger(`clip-path:${cs.clipPath}`);
        if (rect.width < MIN_VISIBLE_WIDTH_PX) return trigger(`width:${rect.width}`);
        if (rect.height < MIN_VISIBLE_HEIGHT_PX) return trigger(`height:${rect.height}`);
        // font-size collapse hides the viewport-corner text badge specifically.
        if (opts.variant === "viewport-corner" && parseFloat(cs.fontSize) < 6)
            return trigger(`font-size:${cs.fontSize}`);
        // transform: scale(0) reduces the painted area to a point.
        if (cs.transform && cs.transform !== "none") {
            const m = cs.transform.match(/matrix\(([-0-9.,\s]+)\)/);
            if (m) {
                const parts = m[1].split(",").map((s) => parseFloat(s.trim()));
                const a = parts[0];
                const d = parts[3];
                if (Math.abs(a) < 0.05 || Math.abs(d) < 0.05) return trigger(`transform:${cs.transform}`);
            }
        }
    }

    function setupTripwire(): void {
        if (opts.variant !== "viewport-corner") return;
        // Initial check after a frame so layout has settled.
        requestAnimationFrame(checkVisibility);
        tripwireInterval = setInterval(checkVisibility, TRIPWIRE_INTERVAL_MS);
        try {
            intersectionObserver = new IntersectionObserver(() => checkVisibility(), {
                threshold: [0, 0.01, 0.5, 1],
            });
            intersectionObserver.observe(host);
        } catch {
            // IntersectionObserver should be available in all supported browsers
            // (Chrome 51+, Firefox 55+, Safari 12.1+); ignore if not.
        }
    }

    function start(): void {
        setupThemeSync();
        setupTamperProtection();
        setupTripwire();
    }

    function destroy(): void {
        themeObserver?.disconnect();
        parentObserver?.disconnect();
        intersectionObserver?.disconnect();
        if (tripwireInterval !== null) clearInterval(tripwireInterval);
        host.remove();
    }

    return { el: host, start, destroy };
}

function buildShadowCss(variant: BrandingVariant): string {
    // Let font-family / line-height inherit naturally from the host page so
    // the branding visually matches the rest of the viewer (which also inherits).
    const common = `
        :host, :host * {
            box-sizing: border-box;
        }
        .link {
            all: unset;
            cursor: pointer;
            font-size: 12px;
            font-weight: 500;
            color: ${LIGHT_NAME_FILL};
            text-decoration: none;
        }
        :host([data-dark]) .link {
            color: ${DARK_NAME_FILL};
        }
        .logo-name { fill: ${LIGHT_NAME_FILL}; }
        .logo-accent { fill: ${LIGHT_ACCENT_FILL}; font-weight: 700; }
        :host([data-dark]) .logo-name { fill: ${DARK_NAME_FILL}; }
        :host([data-dark]) .logo-accent { fill: ${DARK_ACCENT_FILL}; }
    `;

    if (variant === "viewport-corner") {
        return `${common}
            .link {
                display: inline-block;
                padding: 2px 6px;
                opacity: 0.5;
                text-shadow: 0 1px 2px rgba(255, 255, 255, 0.5);
                white-space: nowrap;
                transition: opacity 0.15s ease;
            }
            .link:hover, .link:focus-visible { opacity: 1.0; }
            :host([data-dark]) .link { text-shadow: 0 1px 2px rgba(0, 0, 0, 0.5); }
            .logo-name, .logo-accent { font-weight: 700; }
            .logo-name { color: ${LIGHT_NAME_FILL}; fill: ${LIGHT_NAME_FILL}; }
            .logo-accent { color: ${LIGHT_ACCENT_FILL}; fill: ${LIGHT_ACCENT_FILL}; }
            :host([data-dark]) .logo-name { color: ${DARK_NAME_FILL}; fill: ${DARK_NAME_FILL}; }
            :host([data-dark]) .logo-accent { color: ${DARK_ACCENT_FILL}; fill: ${DARK_ACCENT_FILL}; }
        `;
    }
    if (variant === "loading-block") {
        return `${common}
            .link {
                display: inline-block;
                margin: 0;
                padding: 0;
            }
            svg {
                display: block;
                width: 142px;
                height: 20px;
            }
        `;
    }
    // spread-indicator (logo only)
    return `${common}
        .link {
            display: inline-block;
            cursor: default;
            pointer-events: none;
            padding: 0;
            margin: 0;
        }
        svg {
            display: block;
            width: 142px;
            height: 20px;
        }
    `;
}
