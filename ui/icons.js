const paths = {
  chat: '<path d="M20 11.4a7.6 7.6 0 0 1-7.6 7.6H5l1.2-3.1A7.6 7.6 0 1 1 20 11.4Z"/><path d="M9 11h6"/>',
  code: '<path d="m8 7-5 5 5 5m8-10 5 5-5 5m-2-13-4 16"/>',
  chevron: '<path d="m7 9 5 5 5-5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M10 3h4l.7 2.6 2.2 1.2 2.5-.7 2 3.5-1.8 1.8v2l1.8 1.9-2 3.4-2.5-.7-2.2 1.2L14 22h-4l-.7-2.8L7 18l-2.4.7-2-3.4 1.8-1.9v-2L2.6 9.6l2-3.5L7 6.8l2.3-1.2L10 3Z"/>',
  panel: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M9 5v14"/>',
  attach: '<path d="M8 13.5 14 7a2.4 2.4 0 0 1 3.4 3.4l-7.3 7.3a4 4 0 0 1-5.6-5.6l8-8a5 5 0 0 1 7.1 7.1l-7.2 7.2"/>',
  send: '<path d="m6 12 6-6 6 6m-6-6v13"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" stroke="none"/>',
  speaker: '<path d="M3 9v6h4l5 4V5L7 9H3Zm13 0a4 4 0 0 1 0 6m2-9a8 8 0 0 1 0 12"/>',
  mic: '<rect x="9" y="3" width="6" height="12" rx="3"/><path d="M5 11v1a7 7 0 0 0 14 0v-1m-7 8v3m-3 0h6"/>',
  play: '<path d="m8 5 12 7-12 7V5Z"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/>',
  branch: '<circle cx="6" cy="6" r="2"/><circle cx="6" cy="18" r="2"/><circle cx="18" cy="7" r="2"/><path d="M6 8v8m12-7a6 6 0 0 1-6 6H6"/>',
  edit: '<path d="m14 5 5 5M5 19l4-1 10-10a2 2 0 0 0-3-3L6 15Z"/>',
  diff: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 10h4m-2-2v4m4 3h4"/>',
  more: '<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>',
  close: '<path d="m6 6 12 12M18 6 6 18"/>',
  server: '<rect x="4" y="4" width="16" height="7" rx="2"/><rect x="4" y="13" width="16" height="7" rx="2"/><path d="M8 7.5h.01M8 16.5h.01m4-9h4m-4 9h4"/>',
  plug: '<path d="M8 3v5m8-5v5M6 8h12v3a6 6 0 0 1-6 6v4m-6-10a6 6 0 0 0 6 6"/>',
  shield: '<path d="M12 22s7-3 7-10V6l-7-3-7 3v6c0 7 7 10 7 10Z"/><path d="m9 12 2 2 4-5"/>',
  palette: '<circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 0 0 16Z" fill="currentColor"/>',
  note: '<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M8 8h8m-8 4h8m-8 4h5"/>',
  check: '<path d="m5 12 4 4L19 6"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  search: '<circle cx="10.5" cy="10.5" r="5.5"/><path d="m15 15 4 4"/>',
  image: '<rect x="4" y="4" width="16" height="16" rx="2"/><circle cx="9" cy="9" r="1.5"/><path d="m4 17 5-5 3 3 3-4 5 6"/>',
  download: '<path d="M12 4v11m-4-4 4 4 4-4M5 16v4h14v-4"/>',
  refresh: '<path d="M20 11a8 8 0 0 0-13.5-5L4 9m0-5v5h5m-5 4a8 8 0 0 0 13.5 5l2.5-3m0 5v-5h-5"/>',
  arrow: '<path d="M6 18 18 6M8 6h10v10"/>',
  terminal: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m7 9 3 3-3 3m6 0h4"/>',
  clock: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4l3 2"/>',
};

export function icon(name, size = 18) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  node.setAttribute('viewBox', '0 0 24 24');
  node.setAttribute('width', size);
  node.setAttribute('height', size);
  node.setAttribute('fill', 'none');
  node.setAttribute('stroke', 'currentColor');
  node.setAttribute('stroke-width', '1.65');
  node.setAttribute('stroke-linecap', 'round');
  node.setAttribute('stroke-linejoin', 'round');
  node.setAttribute('aria-hidden', 'true');
  node.innerHTML = paths[name] ?? '';
  return node;
}
