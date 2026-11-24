// DOM construction keeps model output and endpoint labels out of the HTML parser.
import { icon } from './icons.js';

export const $ = (id) => document.getElementById(id);

export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs)) {
    if (value == null || value === false) continue;
    if (name === 'class') node.className = value;
    else if (name.startsWith('on')) node.addEventListener(name.slice(2), value);
    else if (name === 'value' || name === 'checked' || name === 'disabled') node[name] = value;
    else node.setAttribute(name, value === true ? '' : value);
  }
  for (const child of children.flat(Infinity)) {
    if (child == null || child === false) continue;
    node.append(child instanceof Node ? child : String(child));
  }
  return node;
}

export function button(label, handler, opts = {}) {
  const { glyph, style = 'button', title = label, disabled = false, onlyIcon = false } = opts;
  return el('button', {
    type: 'button', class: style, title, disabled, onclick: handler, 'aria-label': onlyIcon ? label : null,
  }, glyph && icon(glyph, 17), onlyIcon ? null : label);
}

export function options(items, value, onChange, attrs = {}) {
  const select = el('select', { ...attrs, onchange: (event) => onChange?.(event.target.value) },
    items.map(([key, label]) => el('option', { value: key }, label)));
  select.value = value ?? '';
  return select;
}

export function field(label, input, note = '') {
  return el('label', { class: 'field' }, el('span', { class: 'field-label' }, label), input,
    note ? el('span', { class: 'field-note' }, note) : null);
}

export function check(label, value, onChange) {
  return el('label', { class: 'check' },
    el('input', { type: 'checkbox', checked: value, onchange: (event) => onChange(event.target.checked) }), label);
}

export function uuid() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-');
}

export const lines = (text) => text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);

function inline(text) {
  const pattern = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^\s)]+\))/g;
  const output = [];
  let previous = 0;
  for (const match of text.matchAll(pattern)) {
    output.push(text.slice(previous, match.index));
    const token = match[0];
    if (token.startsWith('`')) output.push(el('code', {}, token.slice(1, -1)));
    else if (token.startsWith('**')) output.push(el('strong', {}, token.slice(2, -2)));
    else if (token.startsWith('*')) output.push(el('em', {}, token.slice(1, -1)));
    else {
      const [, label, url] = /^\[([^\]]+)\]\((.+)\)$/.exec(token);
      // An explicit copy action keeps generated URLs from navigating the app webview.
      output.push(el('button', { class: 'text-link', type: 'button', title: 'Copy ' + url,
        onclick: () => navigator.clipboard.writeText(url).catch(() => {}) }, label, icon('copy', 12)));
    }
    previous = match.index + token.length;
  }
  output.push(text.slice(previous));
  return output;
}

export function markdown(source, copy) {
  const result = el('div', { class: 'markdown' });
  const rows = source.replaceAll('\r\n', '\n').split('\n');
  for (let i = 0; i < rows.length;) {
    const line = rows[i++];
    if (!line.trim()) continue;
    const fence = /^\s*```(.*)$/.exec(line);
    if (fence) {
      const block = [];
      while (i < rows.length && !/^\s*```\s*$/.test(rows[i])) block.push(rows[i++]);
      if (i < rows.length) i++;
      const raw = block.join('\n');
      result.append(el('div', { class: 'code-block' },
        el('div', { class: 'code-header' }, fence[1].trim() || 'Code',
          button('Copy code', () => copy(raw), { glyph: 'copy', onlyIcon: true, style: 'icon-button' })),
        el('pre', {}, el('code', {}, raw))));
      continue;
    }
    const heading = /^(#{1,6})\s+(.+)$/.exec(line);
    if (heading) {
      result.append(el('h' + Math.min(heading[1].length + 1, 6), {}, inline(heading[2])));
      continue;
    }
    const list = /^\s*([-+*]|\d+[.)])\s+(.+)$/.exec(line);
    if (list) {
      const node = el(/\d/.test(list[1]) ? 'ol' : 'ul', {}, el('li', {}, inline(list[2])));
      while (i < rows.length) {
        const next = /^\s*([-+*]|\d+[.)])\s+(.+)$/.exec(rows[i]);
        if (!next || /\d/.test(next[1]) !== /\d/.test(list[1])) break;
        node.append(el('li', {}, inline(next[2])));
        i++;
      }
      result.append(node);
      continue;
    }
    if (line.startsWith('> ')) {
      result.append(el('blockquote', {}, inline(line.slice(2))));
      continue;
    }
    if (line.includes('|') && /^\s*\|?\s*:?-{2,}/.test(rows[i] ?? '')) {
      const cells = (row) => row.trim().replace(/^\||\|$/g, '').split('|').map((cell) => cell.trim());
      const head = el('thead', {}, el('tr', {}, cells(line).map((cell) => el('th', {}, inline(cell)))));
      const body = el('tbody');
      i++;
      while (i < rows.length && rows[i].includes('|')) {
        body.append(el('tr', {}, cells(rows[i++]).map((cell) => el('td', {}, inline(cell)))));
      }
      result.append(el('div', { class: 'table-wrap' }, el('table', {}, head, body)));
      continue;
    }
    const paragraph = [line];
    while (i < rows.length && rows[i].trim() && !/^\s*(```|#{1,6} |[-+*] |\d+[.)] |>|\|)/.test(rows[i])) {
      paragraph.push(rows[i++]);
    }
    result.append(el('p', {}, inline(paragraph.join('\n'))));
  }
  return result;
}
