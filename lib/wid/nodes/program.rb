module Wid
  module Nodes
    class Program < Node
      attr_reader :body

      def initialize(body)
        @body = body
      end

      def children
        @body
      end

      def to_hash
        {class: self.class, body: @body.map(&:to_hash)}
      end
    end
  end
end
