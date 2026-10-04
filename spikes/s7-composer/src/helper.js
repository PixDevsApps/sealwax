(() => {
  'use strict';
  if (window.mailComposer) {
    return;
  }
  const handlers = window.webkit && window.webkit.messageHandlers;
  const post = (message) => {
    if (handlers && handlers.composer) {
      handlers.composer.postMessage(message);
    }
  };

  const ALPHABET = 'abcdefghijklmnopqrstuvwxyz234567';
  const token = () => {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    let bits = 0;
    let pending = 0;
    let out = '';
    for (const byte of bytes) {
      bits = (bits << 8) | byte;
      pending += 8;
      while (pending >= 5) {
        pending -= 5;
        out += ALPHABET[(bits >> pending) & 31];
      }
      bits &= (1 << pending) - 1;
    }
    if (pending > 0) {
      out += ALPHABET[(bits << (5 - pending)) & 31];
    }
    return out;
  };

  const DROP = new Set([
    'SCRIPT', 'STYLE', 'IFRAME', 'FRAME', 'FRAMESET', 'OBJECT', 'EMBED', 'APPLET', 'FORM',
    'INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP', 'LABEL', 'FIELDSET',
    'LEGEND', 'DATALIST', 'OUTPUT', 'METER', 'PROGRESS', 'LINK', 'META', 'BASE', 'HEAD',
    'TITLE', 'SVG', 'MATH', 'TEMPLATE', 'NOSCRIPT', 'NOEMBED', 'NOFRAMES', 'AUDIO', 'VIDEO',
    'SOURCE', 'TRACK', 'CANVAS', 'PICTURE', 'MAP', 'AREA', 'DIALOG', 'SLOT', 'PORTAL', 'PARAM',
    'XMP', 'PLAINTEXT',
  ]);
  const KEEP = new Set([
    'P', 'DIV', 'BR', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'DEL', 'SUB', 'SUP', 'A',
    'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE', 'CODE', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HR',
    'TABLE', 'THEAD', 'TBODY', 'TFOOT', 'TR', 'TD', 'TH', 'CAPTION', 'IMG',
  ]);
  const BLOCKS = new Set([
    'SECTION', 'ARTICLE', 'HEADER', 'FOOTER', 'NAV', 'MAIN', 'ASIDE', 'FIGURE', 'FIGCAPTION',
    'ADDRESS', 'DETAILS', 'SUMMARY', 'CENTER', 'DL', 'DT', 'DD', 'HGROUP',
  ]);
  const VOID = new Set(['BR', 'HR', 'IMG']);
  const SEMANTIC = { B: 'B', STRONG: 'B', I: 'I', EM: 'I', U: 'U', S: 'S', STRIKE: 'S', DEL: 'S' };
  const IMAGE_SOURCE = /^data:image\/(png|jpeg|gif|webp);base64,[a-z0-9+/=\s]*$/i;
  const LINK = /^(https?:\/\/|mailto:)/i;
  const REMOTE = /^\s*(https?:)?\/\//i;
  const URL_ATTRIBUTE = /^(src|href|background|poster|srcset|action|formaction|data|codebase|cite|longdesc|lowsrc|dynsrc|ping|xlink:href)$/;
  const INDENT_STYLE = 'margin: 0 0 0 40px; border: none; padding: 0px;';
  const QUOTE_STYLE = 'margin:0 0 0 0.8ex;border-left:2px solid #8a8a8a;padding-left:1ex';
  const MAX_IMAGE = 10 * 1024 * 1024;

  const emptyStats = () => ({
    dropped: {}, handlers: 0, styles: 0, attributes: 0, remote: 0, images: 0, links: 0,
    comments: 0, unwrapped: 0, deep: 0,
  });

  const emphasis = (element) => {
    if (!element.getAttribute('style')) {
      return [];
    }
    const style = element.style;
    const wrappers = [];
    const weight = style.fontWeight;
    if (weight === 'bold' || weight === 'bolder' || Number(weight) >= 600) {
      wrappers.push('B');
    }
    if (style.fontStyle === 'italic' || style.fontStyle === 'oblique') {
      wrappers.push('I');
    }
    const lines = `${style.textDecorationLine || ''} ${style.textDecoration || ''}`;
    if (lines.includes('underline')) {
      wrappers.push('U');
    }
    if (lines.includes('line-through')) {
      wrappers.push('S');
    }
    return wrappers;
  };

  const normalWeight = (element) => {
    const weight = element.style && element.style.fontWeight;
    return weight === 'normal' || weight === '400' || weight === 'lighter';
  };

  const countAttributes = (element, stats) => {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      if (name.startsWith('on')) {
        stats.handlers += 1;
      } else if (name === 'style') {
        stats.styles += 1;
      } else {
        if (URL_ATTRIBUTE.test(name) && REMOTE.test(attribute.value)) {
          stats.remote += 1;
        }
        stats.attributes += 1;
      }
    }
  };

  const copyElement = (source, tag, doc, stats) => {
    const copy = doc.createElement(tag === 'STRONG' ? 'B' : tag === 'EM' ? 'I' : tag);
    for (const attribute of Array.from(source.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value.trim();
      if (name.startsWith('on')) {
        stats.handlers += 1;
      } else if (tag === 'A' && name === 'href') {
        if (LINK.test(value)) {
          copy.setAttribute('href', value);
        } else {
          stats.links += 1;
        }
      } else if (tag === 'IMG' && name === 'src') {
        if (IMAGE_SOURCE.test(value)) {
          copy.setAttribute('src', value);
        } else if (REMOTE.test(value)) {
          stats.remote += 1;
        } else {
          stats.images += 1;
        }
      } else if (tag === 'IMG' && name === 'alt') {
        copy.setAttribute('alt', value);
      } else if (tag === 'IMG' && name === 'data-cid' && /^[a-z2-7]{26}$/.test(value)) {
        copy.setAttribute('data-cid', value);
      } else if (tag === 'IMG' && name === 'data-name') {
        copy.setAttribute('data-name', value.slice(0, 200));
      } else if (tag === 'IMG' && (name === 'width' || name === 'height') && /^\d{1,5}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if ((tag === 'TD' || tag === 'TH') && (name === 'colspan' || name === 'rowspan') && /^\d{1,3}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if (tag === 'OL' && name === 'start' && /^-?\d{1,6}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if (tag === 'BLOCKQUOTE' && name === 'style') {
        if (/margin/.test(value) && /40px/.test(value)) {
          copy.setAttribute('style', INDENT_STYLE);
        } else {
          stats.styles += 1;
        }
      } else if (name === 'style') {
        stats.styles += 1;
      } else {
        if (URL_ATTRIBUTE.test(name) && REMOTE.test(value)) {
          stats.remote += 1;
        }
        stats.attributes += 1;
      }
    }
    if (tag === 'IMG') {
      if (!copy.hasAttribute('src')) {
        return null;
      }
      if (!copy.hasAttribute('data-cid')) {
        copy.setAttribute('data-cid', token());
      }
    }
    return copy;
  };

  const cleanInto = (source, target, doc, stats, depth) => {
    if (depth > 64) {
      stats.deep += 1;
      return;
    }
    for (const node of Array.from(source.childNodes)) {
      if (node.nodeType === Node.TEXT_NODE) {
        target.appendChild(doc.createTextNode(node.data));
        continue;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) {
        stats.comments += 1;
        continue;
      }
      const tag = node.tagName.toUpperCase();
      if (DROP.has(tag)) {
        stats.dropped[tag] = (stats.dropped[tag] || 0) + 1;
        countAttributes(node, stats);
        for (const inner of node.querySelectorAll('*')) {
          countAttributes(inner, stats);
        }
        continue;
      }
      let parent = target;
      for (const wrapper of emphasis(node)) {
        if (wrapper !== SEMANTIC[tag]) {
          const element = doc.createElement(wrapper);
          parent.appendChild(element);
          parent = element;
        }
      }
      if ((tag === 'B' || tag === 'STRONG') && normalWeight(node)) {
        stats.unwrapped += 1;
        countAttributes(node, stats);
        cleanInto(node, parent, doc, stats, depth + 1);
      } else if (BLOCKS.has(tag)) {
        const block = doc.createElement('DIV');
        countAttributes(node, stats);
        parent.appendChild(block);
        cleanInto(node, block, doc, stats, depth + 1);
      } else if (!KEEP.has(tag)) {
        stats.unwrapped += 1;
        countAttributes(node, stats);
        cleanInto(node, parent, doc, stats, depth + 1);
      } else {
        const copy = copyElement(node, tag, doc, stats);
        if (copy) {
          parent.appendChild(copy);
          if (!VOID.has(tag)) {
            cleanInto(node, copy, doc, stats, depth + 1);
          }
        }
      }
    }
  };

  const clean = (html) => {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const stats = emptyStats();
    for (const element of doc.head ? Array.from(doc.head.children) : []) {
      stats.dropped[element.tagName] = (stats.dropped[element.tagName] || 0) + 1;
    }
    const out = doc.createElement('div');
    cleanInto(doc.body, out, doc, stats, 0);
    return { html: out.innerHTML, stats };
  };

  const escapeAttribute = (value) => value
    .replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

  const readAsDataUrl = (file) => new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener('load', () => resolve(reader.result));
    reader.addEventListener('error', () => reject(reader.error));
    reader.readAsDataURL(file);
  });

  const insertFiles = async (files, report) => {
    for (const file of files) {
      if (!/^image\/(png|jpeg|gif|webp)$/.test(file.type)) {
        post({ type: 'attach', name: file.name, mime: file.type, size: file.size });
        report.attachments += 1;
        continue;
      }
      if (file.size > MAX_IMAGE) {
        post({ type: 'error', message: `image over ${MAX_IMAGE} bytes not inserted` });
        continue;
      }
      const url = await readAsDataUrl(file);
      const name = file.name ? ` data-name="${escapeAttribute(file.name.slice(0, 200))}"` : '';
      document.execCommand('insertHTML', false, `<img src="${url}" data-cid="${token()}"${name}>`);
      report.images += 1;
    }
  };

  const insertCleaned = (html, report) => {
    const cleaned = clean(html);
    report.stats = cleaned.stats;
    if (cleaned.html) {
      document.execCommand('insertHTML', false, cleaned.html);
    }
  };

  const transferReport = (kind, data) => ({
    type: kind,
    types: Array.from(data.types || []),
    files: (data.files || []).length,
    images: 0,
    attachments: 0,
    mode: 'native',
    stats: null,
  });

  const handleTransfer = (event, data, report) => {
    const files = Array.from(data.files || []);
    if (files.length) {
      event.preventDefault();
      report.mode = 'files';
      insertFiles(files, report).then(() => post(report), (error) => {
        report.mode = `files-failed: ${error}`;
        post(report);
      });
      return;
    }
    const html = report.types.includes('text/html') ? data.getData('text/html') : '';
    if (html) {
      event.preventDefault();
      report.mode = 'html';
      insertCleaned(html, report);
      post(report);
      return;
    }
    const text = data.getData('text/plain');
    if (text) {
      event.preventDefault();
      report.mode = 'text';
      document.execCommand('insertText', false, text);
    }
    post(report);
  };

  document.addEventListener('paste', (event) => {
    if (event.clipboardData) {
      handleTransfer(event, event.clipboardData, transferReport('paste', event.clipboardData));
    }
  }, true);

  // WebKitGTK exposes neither types nor files for an image-only clipboard; its native paste
  // inserts <img src="blob:…">, which the CSP keeps from loading. Turn those into data: images.
  const adoptBlobImages = async () => {
    for (const image of Array.from(document.querySelectorAll('img[src^="blob:"]'))) {
      try {
        const blob = await (await fetch(image.getAttribute('src'))).blob();
        if (!/^image\/(png|jpeg|gif|webp)$/.test(blob.type) || blob.size > MAX_IMAGE) {
          post({ type: 'error', message: `pasted image of type ${blob.type} (${blob.size} bytes) removed` });
          image.remove();
          continue;
        }
        image.setAttribute('src', await readAsDataUrl(blob));
        image.setAttribute('data-cid', token());
        post({ type: 'adopted', mime: blob.type, size: blob.size });
      } catch (error) {
        post({ type: 'error', message: `pasted image could not be read: ${error}` });
        image.remove();
      }
    }
  };
  document.addEventListener('input', (event) => {
    if (event.inputType === 'insertFromPaste' || event.inputType === 'insertFromDrop') {
      adoptBlobImages();
    }
  }, true);

  let draggingInside = false;
  document.addEventListener('dragstart', () => { draggingInside = true; }, true);
  document.addEventListener('dragend', () => { draggingInside = false; }, true);
  document.addEventListener('dragover', (event) => {
    if (!draggingInside && event.dataTransfer && Array.from(event.dataTransfer.types).includes('Files')) {
      event.preventDefault();
    }
  }, true);
  document.addEventListener('drop', (event) => {
    const data = event.dataTransfer;
    if (!data || draggingInside) {
      return;
    }
    const range = document.caretRangeFromPoint(event.clientX, event.clientY);
    if (range) {
      const selection = document.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    }
    handleTransfer(event, data, transferReport('drop', data));
  }, true);

  const closestElement = (node, selector) => {
    const element = node && node.nodeType === Node.ELEMENT_NODE ? node : node && node.parentElement;
    return element ? element.closest(selector) : null;
  };

  const computeState = () => {
    const selection = document.getSelection();
    const anchor = selection ? selection.anchorNode : null;
    const list = closestElement(anchor, 'ul, ol');
    const link = closestElement(anchor, 'a[href]');
    return {
      type: 'state',
      bulleted: !!list && list.tagName === 'UL',
      numbered: !!list && list.tagName === 'OL',
      quote: !!closestElement(anchor, 'blockquote:not([style])'),
      indent: !!closestElement(anchor, 'blockquote[style]'),
      link: link ? link.getAttribute('href') : '',
      bold: document.queryCommandState('bold'),
      italic: document.queryCommandState('italic'),
      underline: document.queryCommandState('underline'),
      strikethrough: document.queryCommandState('strikeThrough'),
    };
  };

  let lastState = '';
  let stateScheduled = false;
  const scheduleState = () => {
    if (stateScheduled) {
      return;
    }
    stateScheduled = true;
    setTimeout(() => {
      stateScheduled = false;
      const state = computeState();
      const key = JSON.stringify(state);
      if (key !== lastState) {
        lastState = key;
        post(state);
      }
    }, 0);
  };
  document.addEventListener('selectionchange', scheduleState);
  document.addEventListener('input', scheduleState);

  let reportInput = false;
  document.addEventListener('input', (event) => {
    if (reportInput) {
      post({
        type: 'input',
        inputType: event.inputType,
        data: event.data || '',
        composing: event.isComposing,
      });
    }
  }, true);

  const serialize = (domain) => {
    const cleaned = clean(document.body.innerHTML);
    const doc = new DOMParser().parseFromString('<!DOCTYPE html><body></body>', 'text/html');
    const root = doc.body;
    root.innerHTML = cleaned.html;
    const images = [];
    const byToken = new Map();
    for (const image of Array.from(root.querySelectorAll('img'))) {
      const match = /^data:(image\/[a-z]+);base64,(.*)$/is.exec(image.getAttribute('src') || '');
      if (!match) {
        image.remove();
        continue;
      }
      const data = match[2].replace(/\s+/g, '');
      let id = image.getAttribute('data-cid') || token();
      if (byToken.has(id) && byToken.get(id).data !== data) {
        id = token();
      }
      if (!byToken.has(id)) {
        const entry = {
          cid: `${id}@${domain}`,
          mime: match[1].toLowerCase(),
          data,
          name: image.getAttribute('data-name') || '',
        };
        byToken.set(id, entry);
        images.push(entry);
      }
      image.removeAttribute('data-cid');
      image.removeAttribute('data-name');
      image.setAttribute('src', `cid:${byToken.get(id).cid}`);
    }
    for (const quote of Array.from(root.querySelectorAll('blockquote'))) {
      if (!quote.hasAttribute('style')) {
        quote.setAttribute('type', 'cite');
        quote.setAttribute('style', QUOTE_STYLE);
      }
    }
    return { html: root.innerHTML, images, stats: cleaned.stats };
  };

  const textNodes = () => {
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) {
      nodes.push(walker.currentNode);
    }
    return nodes;
  };

  const select = (needle, collapse, offset) => {
    for (const node of textNodes()) {
      const at = node.data.indexOf(needle);
      if (at >= 0) {
        const range = document.createRange();
        if (collapse) {
          range.setStart(node, at + offset);
          range.collapse(true);
        } else {
          range.setStart(node, at);
          range.setEnd(node, at + needle.length);
        }
        const selection = document.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return true;
      }
    }
    return false;
  };

  window.mailComposer = {
    serialize,
    clean,
    state: computeState,
    html: () => document.body.innerHTML,
    insertCleaned: (html) => {
      const report = { stats: null };
      insertCleaned(html, report);
      return report.stats;
    },
    setContent: (html) => {
      document.body.innerHTML = html;
      lastState = '';
      scheduleState();
    },
    selectText: (needle) => select(needle, false, 0),
    selectLink: () => {
      const selection = document.getSelection();
      const link = closestElement(selection ? selection.anchorNode : null, 'a[href]');
      if (!link) {
        return false;
      }
      const range = document.createRange();
      range.selectNodeContents(link);
      selection.removeAllRanges();
      selection.addRange(range);
      return true;
    },
    selectBetween: (start, end) => {
      let first = null;
      let firstOffset = 0;
      for (const node of textNodes()) {
        if (!first) {
          const at = node.data.indexOf(start);
          if (at < 0) {
            continue;
          }
          first = node;
          firstOffset = at;
        }
        const at = node.data.indexOf(end, node === first ? firstOffset : 0);
        if (at >= 0) {
          const range = document.createRange();
          range.setStart(first, firstOffset);
          range.setEnd(node, at + end.length);
          const selection = document.getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
          return true;
        }
      }
      return false;
    },
    caretAt: (needle, offset) => select(needle, true, offset),
    caretAtEnd: () => {
      const selection = document.getSelection();
      selection.selectAllChildren(document.body);
      selection.collapseToEnd();
      return true;
    },
    reportInput: (enabled) => {
      reportInput = enabled;
      return reportInput;
    },
    testDrop: (name, mime, base64) => {
      const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
      const transfer = new DataTransfer();
      transfer.items.add(new File([bytes], name, { type: mime }));
      const rect = document.body.getBoundingClientRect();
      const event = new DragEvent('drop', {
        bubbles: true,
        cancelable: true,
        dataTransfer: transfer,
        clientX: rect.left + 10,
        clientY: rect.top + 10,
      });
      document.body.dispatchEvent(event);
      return event.defaultPrevented;
    },
  };

  post({ type: 'ready', editable: document.body ? document.body.isContentEditable : false });
  scheduleState();
})();
