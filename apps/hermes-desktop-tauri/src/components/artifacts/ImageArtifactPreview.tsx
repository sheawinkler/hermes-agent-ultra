interface ImageArtifactPreviewProps {
  src: string
  onDownload?: () => void
}

export function ImageArtifactPreview({ src, onDownload }: ImageArtifactPreviewProps) {
  return (
    <figure className="terra-image-artifact-preview">
      <img src={src} alt="" />
      <button type="button" onClick={onDownload}>Download</button>
    </figure>
  )
}

export default ImageArtifactPreview
