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
  };
  slider.addEventListener("input", update);
  update();
}

function fail(msg) {
  $("loading").hidden = true;
  const err = $("error");
  err.hidden = false;
  err.textContent = msg;
}

main();
