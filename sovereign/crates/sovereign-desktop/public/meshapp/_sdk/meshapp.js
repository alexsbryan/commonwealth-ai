// MeshApp SDK — barrel. A bundle imports everything it needs from here:
//
//   import { connect, scaleBanner, forceGraph, entityDetail } from "../_sdk/meshapp.js";
//
// Dependency-free ES modules served from the same origin (CSP `script-src
// 'self'`). Pair with `<link rel="stylesheet" href="../_sdk/meshapp.css">`.
// The SDK turns a mesh-app bundle from ~600 lines of hand-rolled DOM into a
// short composition over the host's permission-gated `window.meshApp` bridge.

export { $, clear, el, append, svg, emsg, fmtInt } from "./dom.js";
export { hasBridge, connect, describe } from "./bridge.js";
export { forceGraph } from "./graph.js";
export { citationExpander, citedEdge, entityDetail } from "./detail.js";
export {
  scaleBanner, typeToggle, searchBox, threadList, barList,
  timelineChart, monthLabel, reconciliationList,
} from "./views.js";
