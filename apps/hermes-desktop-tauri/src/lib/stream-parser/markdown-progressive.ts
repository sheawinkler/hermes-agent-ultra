export interface MarkdownProgressiveResult {
  html: string
  incomplete: boolean
}

export function renderProgressiveMarkdown(source: string): MarkdownProgressiveResult {
  const fenceCount = (source.match(/```/g) ?? []).length
  const incomplete = fenceCount % 2 === 1
  const display = incomplete ? `${source}\n\`\`\`` : source
  return { html: display, incomplete }
}
