// Every page's script: the phone's menu, and the clips that play while seen.
// Loaded in <head> without defer, so the first line runs before any paint.

// The stylesheet folds the phone's rail away only under `.js`: with scripts
// off, the rail stays open and every link is reachable.
document.documentElement.classList.add('js');

document.addEventListener('DOMContentLoaded', () => {
  // The menu button: the rail opens under the bar, and closes on a second
  // press, on Escape, or when a link in it is followed.
  const burger = document.querySelector('.burger');
  const rail = document.getElementById('rail');
  const open = (on) => {
    rail.classList.toggle('open', on);
    burger.setAttribute('aria-expanded', String(on));
    document.body.classList.toggle('menuopen', on);
  };
  burger.addEventListener('click', () => open(!rail.classList.contains('open')));
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') open(false); });
  rail.addEventListener('click', (e) => { if (e.target.closest('a')) open(false); });
  // The current page's button, scrolled into view in a long rail.
  rail.querySelector('[aria-current="page"]')?.scrollIntoView({ block: 'nearest' });

  // A clip plays only while on screen: `preload="none"` in the markup, so a
  // page of them costs nothing until one is scrolled to. Someone who asked
  // their system for less motion gets the poster and the controls instead.
  const videos = document.querySelectorAll('figure.media video');
  if (matchMedia('(prefers-reduced-motion: reduce)').matches || !('IntersectionObserver' in window)) {
    for (const v of videos) v.controls = true;
    return;
  }
  const seen = new IntersectionObserver((entries) => {
    for (const { target, isIntersecting } of entries) {
      if (isIntersecting) target.play().catch(() => { target.controls = true; });
      else target.pause();
    }
  }, { threshold: 0.4 });
  for (const v of videos) seen.observe(v);
});
