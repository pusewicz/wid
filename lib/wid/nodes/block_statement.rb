module Wid
  module Nodes
    class BlockStatement < Node
      attr_reader :statements

      def initialize(statements)
        @statements = statements
        super
      end

      def children
        @statements
      end

      def to_hash
        {class: self.class, statements: @statements.map(&:to_hash)}
      end
    end
  end
end
