// Shared pure logic for the composer provider/model picker, kept separate from
// DOM construction so both sides can be tested independently.
export function filterComposerChoices(connectedProviders, {
  selectedProviderId,
  selectedModelId,
  query = '',
} = {}) {
  const text = query.trim().toLocaleLowerCase();
  const matches = (value) => text ? `${value}`.toLocaleLowerCase().includes(text) : true;
  const providers = connectedProviders.filter((provider) =>
    matches(provider.name) || provider.models.some(matches));
  const provider = providers.find((item) => item.id === selectedProviderId) ?? providers[0];
  const models = (provider?.models ?? []).filter(matches);
  const selectedModel = (provider?.models ?? []).includes(selectedModelId) ? selectedModelId : models[0] ?? '';
  return { providers, provider, models, selectedModel };
}
