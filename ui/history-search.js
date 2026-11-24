// Search the saved local transcript, navigation labels, selected provider,
// and selected model. Credential values and attachment contents stay excluded.
export function matchesHistory(thread, project, query, provider) {
  const text = query.trim().toLocaleLowerCase();
  if (!text) return true;
  const messages = thread?.messages ?? [];
  const labels = [
    thread?.title, project?.name, provider?.name, thread?.model_id,
    ...messages.map((message) => message.text),
    ...messages.flatMap((message) => (message.attachments ?? []).map((item) => item.name)),
    ...messages.flatMap((message) => (message.resources ?? []).map((item) => item.name)),
    ...messages.flatMap((message) => (message.resources ?? []).map((item) => item.uri)),
    ...messages.flatMap((message) => [message.model, message.tool_label, message.tool_name]),
  ];
  return labels
    .some((value) => typeof value === 'string' && value.toLocaleLowerCase().includes(text));
}
