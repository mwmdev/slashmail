const copyTimers = new WeakMap();

function resetCopyState(element) {
  element.classList.remove('is-copied', 'copy-failed');
}

function scheduleCopyReset(element, label) {
  window.clearTimeout(copyTimers.get(element));
  copyTimers.set(element, window.setTimeout(() => {
    resetCopyState(element);
    element.setAttribute('aria-label', label);
  }, 1600));
}

document.querySelectorAll('.copy-block').forEach((block) => {
  const heading = block.closest('.setup-step, .tutorial-step')?.querySelector('h3').textContent ?? 'Install';
  const label = `Copy ${heading} command`;
  block.setAttribute('role', 'button');
  block.setAttribute('tabindex', '0');
  block.setAttribute('aria-label', label);
  block.setAttribute('title', label);

  const copyCommand = async () => {
    resetCopyState(block);
    try {
      await navigator.clipboard.writeText(block.textContent.trim());
      block.classList.add('is-copied');
      block.setAttribute('aria-label', `${heading} command copied`);
    } catch {
      block.classList.add('copy-failed');
      block.setAttribute('aria-label', `Could not copy ${heading} command`);
    }
    scheduleCopyReset(block, label);
  };

  block.addEventListener('click', copyCommand);
  block.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      copyCommand();
    }
  });
});

const toTop = document.querySelector('.to-top');
const toggleToTop = () => toTop.classList.toggle('is-visible', window.scrollY > 900);
window.addEventListener('scroll', toggleToTop, { passive: true });
toggleToTop();

document.querySelectorAll('.nav-menu').forEach((menu) => {
  const button = menu.querySelector('.nav-menu-button');
  const setOpen = (open) => {
    button.setAttribute('aria-expanded', open);
    menu.classList.toggle('is-open', open);
  };

  button.addEventListener('click', () => setOpen(button.getAttribute('aria-expanded') !== 'true'));
  menu.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    setOpen(false);
    button.focus();
  });
  menu.addEventListener('focusout', (event) => {
    if (!menu.contains(event.relatedTarget)) setOpen(false);
  });
  menu.querySelectorAll('a').forEach((link) => link.addEventListener('click', () => setOpen(false)));
  document.addEventListener('click', (event) => {
    if (!menu.contains(event.target)) setOpen(false);
  });
});
