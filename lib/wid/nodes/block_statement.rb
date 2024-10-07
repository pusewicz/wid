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
    end
  end
end
