/**
 * Font resolver: maps common document font names to Google Fonts substitutes.
 *
 * Mechanism (same as docmentis): instead of pre-bundling fonts, fetch them
 * on-demand when the document declares them. This ensures any document from
 * any user renders with the correct (or best-equivalent) font metrics.
 *
 * Priority: exact match on Google Fonts > metric-compatible substitute > skip.
 *
 * Metric-compatible fonts (same character advance widths):
 *  Calibri      → Carlito    (OFL, drop-in replacement)
 *  Arial        → Arimo      (OFL, drop-in replacement)
 *  Times New Roman → Tinos   (OFL, drop-in replacement)
 *  Courier New  → Cousine    (OFL, drop-in replacement)
 *  Cambria      → Caladea    (OFL, close replacement)
 */

// Map from document font name → Google Fonts family name.
// Key: as it appears in w:rFonts w:ascii (case-insensitive match is done at call site)
const FONT_MAP: Record<string, string> = {
    // Microsoft Office defaults - metric-compatible substitutes
    "calibri": "Carlito",
    "calibri light": "Carlito",
    "arial": "Arimo",
    "arial narrow": "Arimo",
    "times new roman": "Tinos",
    "courier new": "Cousine",
    "cambria": "Caladea",
    "cambria math": "Caladea",
    "georgia": "Tinos",
    "trebuchet ms": "Open Sans",
    "verdana": "Open Sans",
    "garamond": "EB Garamond",
    "palatino linotype": "IM Fell English",
    "palatino": "IM Fell English",
    "book antiqua": "IM Fell English",
    "gill sans mt": "Cabin",
    "century gothic": "Questrial",
    "franklin gothic medium": "Barlow",
    "segoe ui": "Noto Sans",
    "tahoma": "Noto Sans",

    // Google Fonts (already available natively)
    "roboto": "Roboto",
    "open sans": "Open Sans",
    "lato": "Lato",
    "montserrat": "Montserrat",
    "source sans pro": "Source Sans 3",
    "source sans 3": "Source Sans 3",
    "ubuntu": "Ubuntu",
    "merriweather": "Merriweather",
    "playfair display": "Playfair Display",
    "raleway": "Raleway",
    "poppins": "Poppins",
    "nunito": "Nunito",
    "inter": "Inter",
    "noto sans": "Noto Sans",
    "noto serif": "Noto Serif",
    "pt sans": "PT Sans",
    "pt serif": "PT Serif",
    "josefin sans": "Josefin Sans",
    "barlow": "Barlow",
    "cabin": "Cabin",
    "karla": "Karla",
    "dosis": "Dosis",
    "fira sans": "Fira Sans",
    "oxygen": "Oxygen",
    "exo 2": "Exo 2",
    "exo": "Exo 2",
    "mulish": "Mulish",
    "heebo": "Heebo",
    "libre franklin": "Libre Franklin",
    "libre baskerville": "Libre Baskerville",
    "crimson text": "Crimson Text",
    "cormorant": "Cormorant Garamond",
};

// Simple in-memory cache to avoid refetching the same font.
const fetchCache = new Map<string, Uint8Array | null>();

/**
 * Fetch a Google Font TTF/OTF from the Google Fonts CSS v2 API.
 * Returns font bytes or null on failure.
 */
async function fetchGoogleFont(family: string, weight = 400, italic = false): Promise<Uint8Array | null> {
    const cacheKey = `${family}:${weight}:${italic}`;
    if (fetchCache.has(cacheKey)) return fetchCache.get(cacheKey)!;

    try {
        const ital = italic ? "1" : "0";
        const cssUrl = `https://fonts.googleapis.com/css2?family=${encodeURIComponent(family)}:ital,wght@${ital},${weight}&display=swap`;
        const cssResp = await fetch(cssUrl, {
            headers: { "User-Agent": "Mozilla/5.0" },
        });
        if (!cssResp.ok) {
            fetchCache.set(cacheKey, null);
            return null;
        }
        const css = await cssResp.text();
        // Extract the first font URL from the CSS
        const urlMatch = css.match(/url\((https:\/\/fonts\.gstatic\.com\/[^)]+\.(?:ttf|woff2|woff|otf))\)/);
        if (!urlMatch) {
            fetchCache.set(cacheKey, null);
            return null;
        }
        const fontResp = await fetch(urlMatch[1]);
        if (!fontResp.ok) {
            fetchCache.set(cacheKey, null);
            return null;
        }
        const bytes = new Uint8Array(await fontResp.arrayBuffer());
        fetchCache.set(cacheKey, bytes);
        return bytes;
    } catch {
        fetchCache.set(cacheKey, null);
        return null;
    }
}

/**
 * Resolve and prefetch fonts declared in a document.
 *
 * For each declared font name, looks up a Google Fonts equivalent and
 * fetches Regular + Bold weights. Returns a list of font byte arrays
 * ready to register with the WASM engine.
 *
 * @param declaredFonts - Font names from get_declared_fonts()
 * @returns Array of fetched font byte arrays
 */
export async function resolveFonts(declaredFonts: string[]): Promise<Uint8Array[]> {
    const seen = new Set<string>();
    const fetches: Promise<Uint8Array | null>[] = [];

    for (const fontName of declaredFonts) {
        const mapped = FONT_MAP[fontName.toLowerCase()];
        if (!mapped || seen.has(mapped)) continue;
        seen.add(mapped);
        // Fetch regular and bold weights for this family
        fetches.push(fetchGoogleFont(mapped, 400, false));
        fetches.push(fetchGoogleFont(mapped, 700, false));
        fetches.push(fetchGoogleFont(mapped, 400, true));
    }

    const results = await Promise.all(fetches);
    return results.filter((b): b is Uint8Array => b !== null);
}
