// Evaluated on demand, never polled during rendering. Element references belong
// to one snapshot and one document; a changed target must be inspected again.
(() => {
  const command = __ZERON_COMMAND__;
  const state = globalThis.__zeronAutomation ||= {version: 0, nonce:Math.random().toString(36).slice(2), nodes:new Map()};
  const clean = text => String(text || '').replace(/\s+/g, ' ').trim().slice(0, 300);
  const name = el => clean(el.getAttribute('aria-label') ||
    (el.getAttribute('aria-labelledby') || '').split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ') ||
    el.labels?.[0]?.textContent || el.getAttribute('alt') || el.getAttribute('placeholder') ||
    (el.matches('input[type=button],input[type=submit],input[type=reset]') ? el.value : '') || el.innerText || el.getAttribute('title'));
  const visible = el => {
    const rect = el.getBoundingClientRect(), style = getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none' && style.opacity !== '0' &&
      !el.closest('[inert],[aria-hidden="true"]');
  };
  const fingerprint = el => [el.tagName, el.getAttribute('type'), name(el), el.getAttribute('href')].join('|');
  const role = el => el.getAttribute('role') || ({A:'link', BUTTON:'button', SUMMARY:'button', INPUT:({checkbox:'checkbox',radio:'radio',button:'button',submit:'button',reset:'button',range:'slider',number:'spinbutton',search:'searchbox'}[el.type] || 'textbox'),
    TEXTAREA:'textbox', SELECT:el.multiple ? 'listbox' : 'combobox', OPTION:'option', IMG:'img', H1:'heading', H2:'heading', H3:'heading', H4:'heading', H5:'heading', H6:'heading'}[el.tagName]) ||
    (el.isContentEditable ? 'textbox' : 'element');
  try {
    if (command.action === 'snapshot') {
      state.nodes.clear();
      state.version++;
      state.url = location.href;
      const token = `${state.nonce}-${performance.timeOrigin}-${state.version}`;
      let count = 0, truncated = false;
      const lines = [], roots = [document];
      while (roots.length && count < 1500) {
        const root = roots.shift();
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
        for (let el = walker.nextNode(); el; el = walker.nextNode()) {
          if (++count > 1500) { truncated = true; break; }
          if (el.shadowRoot) roots.push(el.shadowRoot);
          if (!visible(el)) continue;
          const interactive = el.matches('a[href],button,summary,input:not([type=hidden]),textarea,select,[contenteditable=true],[role],[tabindex]');
          if (!interactive && el.children.length && !el.matches('h1,h2,h3,h4,h5,h6,img,label,p,li,pre,td,th,iframe')) continue;
          const label = name(el);
          if (!interactive && !label && el.tagName !== 'IFRAME') continue;
          const ref = interactive ? `${token}:${state.nodes.size + 1}` : null;
          if (ref) state.nodes.set(ref, {el, fingerprint: fingerprint(el)});
          const value = 'value' in el && el.type !== 'password' ? clean(el.value) : undefined;
          lines.push({role: role(el), name: label, ...(ref && {ref}), ...(value !== undefined && {value}),
            ...(el.tagName === 'A' && {url:el.href}),
            ...(el.disabled && {disabled:true}), ...('checked' in el && {checked:el.checked}),
            ...(el.getRootNode().activeElement === el && {focused:true}),
            ...(el.tagName === 'IFRAME' && {frameUrl:el.src,note:'Frame contents require opening its URL in another tab'})});
        }
      }
      return {url:location.href, title:document.title, elements:lines, truncated};
    }
    let el;
    if (command.ref) {
      const node = state.nodes.get(command.ref);
      if (!node || !node.el.isConnected || state.url !== location.href || node.fingerprint !== fingerprint(node.el))
        throw Error('browser_stale_ref: take a fresh snapshot');
      el = node.el;
      if (!visible(el) || el.disabled || el.getAttribute('aria-disabled') === 'true')
        throw Error('browser_not_actionable: target is hidden or disabled');
      el.scrollIntoView({block:'center', inline:'nearest', behavior:'instant'});
    }
    if (command.action === 'click') {
      const rect = el.getBoundingClientRect();
      const x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
      const hit = el.getRootNode().elementFromPoint(x,y);
      if (!hit || !(el === hit || el.contains(hit))) throw Error('browser_obscured: target is covered');
      return {point:{x,y}};
    }
    if (command.action === 'fill') {
      if (el.readOnly) throw Error('browser_not_editable: target is read-only');
      el.focus();
      if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
        const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, command.value);
      } else if (el.isContentEditable) el.textContent = command.value;
      else throw Error('browser_not_editable: target is not a text field');
      el.dispatchEvent(new Event('input', {bubbles:true}));
      el.dispatchEvent(new Event('change', {bubbles:true}));
      return {filled:true};
    }
    if (command.action === 'select') {
      if (!(el instanceof HTMLSelectElement) || ![...el.options].some(o => o.value === command.value && !o.disabled))
        throw Error('browser_invalid_option: no enabled option with that value');
      el.value = command.value;
      el.dispatchEvent(new Event('input', {bubbles:true}));
      el.dispatchEvent(new Event('change', {bubbles:true}));
      return {selected:command.value};
    }
    if (command.action === 'scroll') { window.scrollBy({left:command.x, top:command.y, behavior:'instant'}); return {x:scrollX,y:scrollY}; }
    throw Error('browser_unsupported_action');
  } catch (error) { return {error:String(error.message || error)}; }
})()
