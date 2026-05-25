<script lang="ts">
  // Sovereign brand mark — "Crinkled Gold" asymmetric pentagon.
  // Source of truth is `src-tauri/icons/icon-source.svg`; this
  // component carries the same polygons inline so the React/Svelte
  // runtime can scale it via `size=` and the surrounding chrome can
  // theme the surrounding glow / breathe animation per surface.
  //
  // When the icon source changes, update both this file and
  // `icon-source.svg` together — the bundle ships the latter as the
  // app launcher icon while this is the inline UI mark.

  interface Props {
    /** Side length in px. Default 64 sits between the toolbar mark
     *  and the empty-state hero size; callers commonly pass 56–96. */
    size?: number;
    /** Decorative role by default — empty-state hero, paired with
     *  a wordmark below. Pass `aria-label` upstream when used in
     *  isolation. */
    title?: string | null;
    /** Extra class hook so callers can layer a breathe animation
     *  or drop-shadow filter without re-defining the SVG. */
    klass?: string;
  }
  let { size = 64, title = null, klass = "" }: Props = $props();
</script>

<svg
  class={`brand-mark ${klass}`}
  width={size}
  height={size}
  viewBox="0 0 1024 1024"
  xmlns="http://www.w3.org/2000/svg"
  role={title ? "img" : "presentation"}
  aria-hidden={title ? undefined : "true"}
  aria-label={title ?? undefined}
>
  <!-- Upper-left cluster: brightest, catching the light source -->
  <polygon points="450,380 310,410 330,285" fill="#DFC068" />
  <polygon points="450,380 170,410 310,410" fill="#F0CC85" />
  <polygon points="310,410 170,410 330,285" fill="#F5E2B0" />
  <!-- Top crown: mid-light, around V1 apex -->
  <polygon points="450,380 490,160 330,285" fill="#DFC068" />
  <polygon points="450,380 660,255 490,160" fill="#DFC068" />
  <!-- Right shoulder cluster -->
  <polygon points="450,380 830,350 660,255" fill="#C9A84C" />
  <polygon points="450,380 650,470 830,350" fill="#C9A84C" />
  <polygon points="450,380 765,590 650,470" fill="#C9A84C" />
  <polygon points="650,470 830,350 765,590" fill="#C9A84C" />
  <!-- The valley: deep shadow where the fold pools -->
  <polygon points="450,380 520,650 765,590" fill="#876122" />
  <polygon points="450,380 205,635 520,650" fill="#876122" />
  <!-- Lower-right: shadow at V3, grading -->
  <polygon points="520,650 765,590 700,830" fill="#876122" />
  <polygon points="520,650 700,830 470,845" fill="#A88838" />
  <!-- Lower-left: mid-shade at V4 -->
  <polygon points="520,650 470,845 240,860" fill="#A88838" />
  <polygon points="520,650 240,860 205,635" fill="#A88838" />
  <!-- Left flank: mid -->
  <polygon points="450,380 170,410 205,635" fill="#C9A84C" />
</svg>

<style>
  .brand-mark {
    display: block;
    /* The icon was designed as silhouette-on-substrate (no backdrop);
       a soft drop shadow lets the gold breathe against any neighbor. */
    filter: drop-shadow(0 0 14px rgba(201, 168, 76, 0.32));
  }
</style>
