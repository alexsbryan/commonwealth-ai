// SF Land-Value-Tax explorer — a first-party mesh app.
//
// Every number rendered here comes from the host's deterministic
// `parcelAnalytics` bridge op (a fold over typed parcel atoms); the only
// arithmetic done in this bundle is `rate × land_base` for the slider,
// and that multiplies a CITED base by a user-chosen rate — it never
// originates a figure. The bundle's sole channel to the host is
// `window.meshApp` (injected by the host; gated by the install grant).

const CORPUS = "sf-assessor-roll";
const TARGET = 1_400_000_000; // ~$1.4B SF business-tax take to replace.
// ≈ SF effective secured property-tax rate — a LABELED estimate, not from
// the roll (which carries assessed values, not tax paid). Used only for the
// per-parcel "current tax" comparison; never an LVT figure.
const PROPERTY_TAX_RATE = 0.0118;
let loadedParcel = null; // the parcel the per-parcel calculator is showing.

const $ = (id) => document.getElementById(id);

function usd(v) {
  const a = Math.abs(v);
  if (a >= 1e9) return "$" + (v / 1e9).toFixed(2) + "B";
  if (a >= 1e6) return "$" + (v / 1e6).toFixed(2) + "M";
  if (a >= 1e3) return "$" + (v / 1e3).toFixed(1) + "K";
  return "$" + v.toFixed(0);
}
const intc = (n) => Number(n).toLocaleString("en-US");

async function main() {
  if (!window.meshApp) {
    return fail("window.meshApp is not available — the host bridge shim did not load.");
  }
  let a;
  try {
    a = await window.meshApp.parcelAnalytics(CORPUS, TARGET);
  } catch (e) {
    return fail(
      "Bridge call failed: " + (e && e.message ? e.message : e) +
      "  (is the LVT app installed with mesh_store_read granted, and is the " +
      CORPUS + " corpus present?)"
    );
  }

  $("loading").hidden = true;
  $("app").hidden = false;
  $("source").textContent = "Source: " + a.corpus_id;

  // Headline land base — cited to the deterministic fold.
  $("land-base").textContent = usd(a.land_value_total);
  $("land-meta").textContent =
    "exact: $" +
    a.land_value_total.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  $("land-chip").textContent = "Σ over " + intc(a.parcel_count) + " parcel atoms";

  $("neutral-rate").textContent = (a.neutral_rate * 100).toFixed(2) + "%";
  $("high").textContent = intc(a.high_land_share_count);
  $("under").textContent = intc(a.underused_count);

  // Verbatim derivation lines from the host (the show-your-work trace).
  for (const line of a.derivation || []) {
    const li = document.createElement("li");
    li.textContent = line;
    $("derivation").appendChild(li);
  }

  // Rate slider: revenue = rate × land_base. The base is cited; the
  // multiply is the only client-side arithmetic.
  const slider = $("rate");
  slider.value = (a.neutral_rate * 100).toFixed(2);
  const update = () => {
    const pct = parseFloat(slider.value);
    const revenue = (pct / 100) * a.land_value_total;
    $("rate-val").textContent = pct.toFixed(2);
    $("revenue").textContent = usd(revenue);
    const delta = revenue - a.business_tax_target;
    $("rate-meta").textContent =
      Math.abs(delta) < a.business_tax_target * 0.01
        ? "≈ revenue-neutral — matches the " + usd(a.business_tax_target) + " target"
        : (delta > 0 ? "surplus " : "shortfall ") +
          usd(Math.abs(delta)) +
          " vs the " + usd(a.business_tax_target) + " target";
    renderParcel(); // keep a loaded parcel's levy in sync with the rate
  };
  slider.addEventListener("input", update);
  update();

  // Per-parcel calculator — find a parcel, then compute at the slider rate.
  $("parcel-search").addEventListener("click", () =>
    searchAndShow($("parcel-query").value.trim()),
  );
  $("parcel-query").addEventListener("keydown", (e) => {
    if (e.key === "Enter") searchAndShow($("parcel-query").value.trim());
  });
}

// Search parcels by street name or number; load the single match, or show
// a clickable pick-list when several match — so a homeowner finds their own
// parcel without knowing its block/lot.
async function searchAndShow(query) {
  if (!query) return;
  $("parcel-error").textContent = "";
  $("parcel-matches").hidden = true;
  let matches;
  try {
    matches = await window.meshApp.searchParcels(CORPUS, query, 25);
  } catch (e) {
    $("parcel-error").textContent = "search failed: " + (e && e.message ? e.message : e);
    return;
  }
  if (!matches || matches.length === 0) {
    $("parcel-result").hidden = true;
    loadedParcel = null;
    $("parcel-error").textContent = "no parcel matching '" + query + "' in " + CORPUS;
    return;
  }
  if (matches.length === 1) {
    loadedParcel = matches[0];
    renderParcel();
    return;
  }
  renderMatches(matches);
  if (matches.length >= 25) {
    $("parcel-error").textContent =
      "Showing the first 25 — add your street number to narrow.";
  }
}

// A clickable pick-list of matches; choosing one loads it into the result.
function renderMatches(matches) {
  const box = $("parcel-matches");
  box.replaceChildren();
  $("parcel-result").hidden = true;
  for (const m of matches) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "match-row";
    const loc = m.attributes.property_location || m.parcel_number;
    const nb = m.attributes.analysis_neighborhood || "";
    row.textContent = [loc, nb, "#" + m.parcel_number].filter(Boolean).join(" · ");
    row.addEventListener("click", () => {
      loadedParcel = m;
      box.hidden = true;
      $("parcel-error").textContent = "";
      renderParcel();
    });
    box.appendChild(row);
  }
  box.hidden = false;
}

// Render the loaded parcel at the macro slider's current rate. The land /
// improvement values are CITED (from the atom); the only arithmetic is
// land × rate and (land+improvement) × the labeled property-tax estimate.
function renderParcel() {
  if (!loadedParcel) return;
  const p = loadedParcel;
  const land = Number(p.attributes.assessed_land_value) || 0;
  const impr = Number(p.attributes.assessed_improvement_value) || 0;
  const rate = parseFloat($("rate").value); // % from the macro slider
  const lvt = (rate / 100) * land;
  const cur = (land + impr) * PROPERTY_TAX_RATE;
  const delta = lvt - cur;

  $("parcel-result").hidden = false;
  $("parcel-loc").textContent = [
    p.attributes.property_location,
    p.attributes.analysis_neighborhood,
    p.attributes.use_definition,
  ]
    .filter(Boolean)
    .join(" · ");
  $("p-plain").textContent =
    "Your land is assessed at " + usd(land) + ". Under a flat " + rate.toFixed(2) +
    "% land tax you'd pay about " + usd(lvt) + "/yr, vs about " + usd(cur) +
    " in property tax today — " +
    (delta <= 0 ? "a saving of " + usd(-delta) : "an increase of " + usd(delta)) + ".";
  $("p-land").textContent = usd(land);
  $("p-impr").textContent = usd(impr);
  $("p-rate").textContent = rate.toFixed(2);
  $("p-lvt").textContent = usd(lvt);
  $("p-cur").textContent = usd(cur);
  const d = $("p-delta");
  if (delta <= 0) {
    d.textContent = "Winner: " + usd(-delta) + " less under LVT";
    d.className = "win";
  } else {
    d.textContent = "Loser: " + usd(delta) + " more under LVT";
    d.className = "lose";
  }
  $("p-chip").textContent =
    "parcel " + p.parcel_number + " · atom " + p.atom_id +
    (p.source_chunk ? " · chunk " + p.source_chunk : "");
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();
