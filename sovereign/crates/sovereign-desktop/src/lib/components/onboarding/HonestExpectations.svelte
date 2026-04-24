<!--
  HonestExpectations — reusable narrative block. Three sections,
  one sharp sentence each. Bottom line up front, in the README's
  plain-spoken voice.

  Pure presentation. No props, no state — just content.
-->
<section class="he">
  <div class="he-col">
    <p class="he-label">Strong at</p>
    <p class="he-line">
      Grounded answers from your content — traced to the exact passage.
    </p>
  </div>
  <div class="he-col">
    <p class="he-label">Weaker at</p>
    <p class="he-line">
      What it hasn't read. No hidden cloud memory.
    </p>
  </div>
  <div class="he-col">
    <p class="he-label">Better with friends</p>
    <p class="he-line">
      Pool GPUs with people you trust. Faster builds, larger corpora, same privacy.
    </p>
  </div>
</section>

<style>
  .he {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 20px;
    padding: 18px 20px;
    background: var(--bg-surface, #191919);
    border: 1px solid var(--border-mid, #333);
    border-radius: 10px;
  }
  .he-col {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .he-label {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent, #c4a46a);
    font-weight: 600;
  }
  .he-line {
    margin: 0;
    font-size: 0.88rem;
    color: var(--text-primary, #eee);
    line-height: 1.5;
    max-width: 32ch;
  }
</style>
