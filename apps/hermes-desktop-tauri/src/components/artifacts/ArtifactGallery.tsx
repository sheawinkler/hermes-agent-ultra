interface ArtifactGalleryProps {
  artifactIds?: string[]
}

export function ArtifactGallery({ artifactIds = [] }: ArtifactGalleryProps) {
  return (
    <aside className="terra-artifact-gallery">
      {artifactIds.map((id) => (
        <div key={id} className="terra-artifact-gallery__item">{id}</div>
      ))}
    </aside>
  )
}

export default ArtifactGallery
