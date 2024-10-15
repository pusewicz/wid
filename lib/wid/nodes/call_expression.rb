module Wid
  module Nodes
    class CallExpression < Expression
      attr_reader :callee, :arguments

      def initialize(callee, arguments)
        @callee = callee
        @arguments = arguments
      end

      def ==(other) = self.class == other.class && callee == other.callee && arguments == other.arguments

      def children
        [@callee, @arguments]
      end

      def to_hash
        {class: self.class, callee: @callee.to_hash, arguments: @arguments&.map(&:to_hash)}
      end
    end
  end
end
