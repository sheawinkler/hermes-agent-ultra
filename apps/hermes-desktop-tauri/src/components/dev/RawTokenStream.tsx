interface RawTokenStreamProps {
  tokens?: string[]
}

export function RawTokenStream({ tokens = [] }: RawTokenStreamProps) {
  return (
    <pre className="terra-raw-token-stream">
      {tokens.join('')}
    </pre>
  )
}

export default RawTokenStream
