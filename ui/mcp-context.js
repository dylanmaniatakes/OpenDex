import { button, el, field } from './dom.js';

// Fetch only the server the user selects. Responses from a closed picker or
// a previous server selection cannot replace the current view.
export function mcpContextPicker({ servers, allowed, invoke, thread, show, close, settings, addResource, insertPrompt }) {
  const panel = el('div', { class: 'details-form mcp-context' });
  show('MCP context', panel);
  if (!allowed || !servers.length) {
    panel.append(el('p', { class: 'muted' }, allowed
      ? 'Enable a server to browse its resources and prompts.'
      : 'The current permission profile disables MCP.'),
    button(allowed ? 'Open MCP settings' : 'Open permissions', () => {
      close();
      settings(allowed ? 'mcp' : 'permissions');
    }));
    return;
  }

  const browser = el('div', { class: 'mcp-browser' });
  const sourceBar = el('div', { class: 'mcp-choice-list' });
  panel.append(sourceBar, browser);
  browser.append(el('p', { class: 'muted' }, 'Choose an MCP server.'));
  let revision = 0;

  function load(container, label, operation, showResult, back) {
    const serial = ++revision;
    const status = el('p', { class: 'muted', role: 'status' }, label);
    container.replaceChildren(status);
    (async () => {
      try {
        const result = await operation();
        if (serial === revision && container.isConnected) showResult(result);
      } catch (error) {
        if (serial === revision && container.isConnected) {
          status.className = 'status-error';
          status.textContent = String(error);
          container.append(el('div', { class: 'form-actions' },
            button('Try again', () => load(container, label, operation, showResult, back)), back && button('Edit arguments', back)));
        }
      }
    })();
  }

  const title = (item) => item.title || item.name;

  function openServer(server) {
    for (const item of sourceBar.children) {
      const selected = item.dataset.server === server.id;
      item.classList.toggle('primary', selected);
      item.setAttribute('aria-pressed', String(selected));
    }
    const request = (command, args = {}) => invoke(command, { thread, id: server.id, ...args });
    load(browser, 'Loading MCP catalog…', () => request('mcp_context_catalog'), (catalog) => {
      const inventory = el('div', { class: 'mcp-inventory' });
      const detail = el('div', { class: 'mcp-preview' });
      const search = el('input', { type: 'search', 'aria-label': 'Filter MCP resources and prompts', placeholder: 'Filter resources and prompts…' });
      browser.replaceChildren(search, inventory, detail);

      function previewResource(uri, name) {
        load(detail, 'Reading resource…', () => request('mcp_read_resource', { uri }), (result) => {
          detail.replaceChildren(
            el('h3', {}, name),
            el('code', { class: 'muted' }, uri),
            el('pre', { class: 'context-text' }, result.text),
            ...(result.binary_count > 0 ? [el('p', { class: 'muted' },
              `${result.binary_count} binary part${result.binary_count === 1 ? '' : 's'} omitted.`)] : []),
            el('div', { class: 'form-actions' }, button('Attach resource', () => addResource({
              server_id: server.id, source: server.name, name, uri, text: result.text,
            }), { glyph: 'plus', style: 'button primary' })),
          );
        });
      }

      function enterUri(template) {
        ++revision;
        const uri = el('input', { placeholder: 'scheme://…', required: true, maxlength: 2048, spellcheck: 'false' });
        detail.replaceChildren(el('form', { class: 'mcp-entry', onsubmit: (event) => {
          event.preventDefault();
          previewResource(uri.value.trim(), template ? title(template) : uri.value.trim());
        } },
        el('h3', {}, template ? title(template) : 'Read a resource URI'),
        template && el('code', { class: 'muted' }, template.uriTemplate),
        field('Resource URI', uri, template ? 'Enter the URI with its template variables filled in.' : ''),
        el('button', { type: 'submit', class: 'button primary' }, 'Preview resource')));
        uri.focus();
      }

      function openPrompt(prompt, values = {}) {
        ++revision;
        const fields = prompt.arguments.map((argument) => ({
          argument,
          input: el('textarea', { rows: 2, required: argument.required, maxlength: 128000 },
            Object.hasOwn(values, argument.name) ? values[argument.name] : ''),
        }));
        const form = el('form', { class: 'mcp-entry', onsubmit: (event) => {
          event.preventDefault();
          const args = Object.fromEntries(fields.filter(({ input }) => input.value !== '')
            .map(({ argument, input }) => [argument.name, input.value]));
          const edit = () => openPrompt(prompt, args);
          load(detail, 'Rendering prompt…', () => request('mcp_get_prompt', { name: prompt.name, arguments: args }), (messages) => {
            detail.replaceChildren(el('h3', {}, title(prompt)), ...messages.map(({ role, text }) => el('div', { class: 'mcp-entry' },
              el('strong', {}, role === 'assistant' ? 'Assistant example' : 'User message'),
              el('pre', { class: 'context-text' }, text))),
            el('div', { class: 'form-actions' },
              button('Edit arguments', edit),
              button('Insert prompt', () => insertPrompt(messages), { glyph: 'plus', style: 'button primary' })));
          }, edit);
        } },
        el('h3', {}, title(prompt)),
        prompt.description && el('p', { class: 'muted' }, prompt.description),
        fields.map(({ argument, input }) => field(argument.name + (argument.required ? ' (required)' : ''), input, argument.description || '')),
        el('button', { type: 'submit', class: 'button primary' }, 'Preview prompt'));
        detail.replaceChildren(form);
      }

      function filter() {
        const searchTerm = search.value.trim().toLowerCase();
        const matches = (item) => [item.name, item.title, item.description, item.uri, item.uriTemplate]
          .some((value) => value?.toLowerCase().includes(searchTerm));
        const groups = [
          ['Resources', catalog.resources, (item) => previewResource(item.uri, title(item))],
          ['Resource templates', catalog.templates, enterUri],
          ['Prompts', catalog.prompts, openPrompt],
        ];
        const sections = groups.flatMap(([heading, items, action]) => {
          const found = items.filter(matches);
          if (!found.length) return [];
          return [el('section', { class: 'mcp-inventory-group' },
            el('h3', {}, heading),
            found.map((item) => el('button', { type: 'button', class: 'catalog-result', onclick: () => action(item) },
              el('span', {}, title(item)),
              item.description && el('small', {}, item.description))))];
        });
        inventory.replaceChildren(...sections);
        if (!sections.length) {
          inventory.append(el('p', { class: 'muted' }, searchTerm ? 'No matching items.' : 'This server has no listed resources or prompts.'));
        }
        if (catalog.can_read_resources) {
          inventory.append(button('Read a resource URI', () => enterUri(null), { glyph: 'note' }));
        }
      }
      search.addEventListener('input', filter);
      filter();
    });
  }

  sourceBar.replaceChildren(...servers.map((server) => {
    const item = button(server.name, () => openServer(server), { glyph: 'plug' });
    item.dataset.server = server.id;
    item.setAttribute('aria-pressed', 'false');
    return item;
  }));
}
