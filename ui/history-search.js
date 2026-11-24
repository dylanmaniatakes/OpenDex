// Search only the saved local transcript and navigation labels.
export function matchesHistory(thread, project, query) {
  const text = query.trim().toLocaleLowerCase();
  if (!text) return true;
  const messages = thread?.messages ?? [];
  const labels = [
    thread?.title, project?.name,
    ...messages.map((message) => message.text),
    ...messages.flatMap((message) => (message.attachments ?? []).map((item) => item.name)),
    ...messages.flatMap((message) => (message.resources ?? []).map((item) => item.name)),
    ...messages.flatMap((message) => (message.resources ?? []).map((item) => item.uri)),
  ];
  return labels
    .some((value) => typeof value === 'string' && value.toLocaleLowerCase().includes(text));
}
