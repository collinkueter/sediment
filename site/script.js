/* Sediment landing — light progressive enhancement.
   Everything degrades gracefully: with JS off, all content is visible. */

(() => {
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---- Scroll reveal via IntersectionObserver ---- */
  const revealers = document.querySelectorAll(".reveal");
  if (reduce || !("IntersectionObserver" in window)) {
    revealers.forEach((el) => el.classList.add("is-in"));
  } else {
    const io = new IntersectionObserver(
      (entries, obs) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          const el = entry.target;
          // Stagger siblings inside a shared container for a settling cascade.
          const sibs = Array.from(el.parentElement?.children || []).filter((n) =>
            n.classList.contains("reveal"),
          );
          const idx = Math.max(0, sibs.indexOf(el));
          el.style.transitionDelay = `${Math.min(idx, 6) * 70}ms`;
          el.classList.add("is-in");
          obs.unobserve(el);
        });
      },
      { rootMargin: "0px 0px -10% 0px", threshold: 0.12 },
    );
    revealers.forEach((el) => io.observe(el));
  }

  /* ---- Sticky nav hairline once scrolled ---- */
  const nav = document.getElementById("nav");
  if (nav) {
    const onScroll = () => nav.classList.toggle("is-stuck", window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
  }

  /* ---- Subtle pointer parallax on the hero stage ---- */
  if (!reduce) {
    const root = document.querySelector("[data-parallax-root]");
    const layers = document.querySelectorAll("[data-parallax]");
    if (root && layers.length) {
      let raf = 0;
      root.addEventListener("pointermove", (e) => {
        const r = root.getBoundingClientRect();
        const dx = (e.clientX - r.left) / r.width - 0.5;
        const dy = (e.clientY - r.top) / r.height - 0.5;
        if (raf) return;
        raf = requestAnimationFrame(() => {
          layers.forEach((l) => {
            const d = parseFloat(l.getAttribute("data-parallax")) || 0;
            l.style.transform = `translate3d(${dx * d}px, ${dy * d}px, 0)`;
          });
          raf = 0;
        });
      });
      root.addEventListener("pointerleave", () => {
        layers.forEach((l) => (l.style.transform = ""));
      });
    }
  }
})();
