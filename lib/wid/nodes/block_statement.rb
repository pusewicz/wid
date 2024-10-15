module Wid
  module Nodes
    class BlockStatement < Node
      attr_reader :statements

      def initialize(body)
        @body = body
        super
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
