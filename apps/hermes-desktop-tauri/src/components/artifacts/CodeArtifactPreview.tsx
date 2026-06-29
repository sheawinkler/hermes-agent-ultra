interface CodeArtifactPreviewProps {
  code: string
  language?: string
}

export function CodeArtifactPreview({ code, language }: CodeArtifactPreviewProps) {
  return (
    <pre className="terra-code-artifact-preview" data-language={language}>
      {code}
    </pre>
  )
}

export default CodeArtifactPreview
