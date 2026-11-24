// Search only the saved local transcript and navigation labels.
export function matchesHistory(thread, project, query) {
  const text = query.trim().toLocaleLowerCase();
  if (!text) return true;
  return [thread?.title, project?.name, ...(thread?.messages ?? []).map((message) => message.text)]
    .some((value) => typeof value === 'string' && value.toLocaleLowerCase().includes(text));
}
