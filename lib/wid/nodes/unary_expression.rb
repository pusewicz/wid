module Wid
  module Nodes
    class UnaryExpression < Expression
      attr_reader :operator, :argument

      def initialize(operator, argument)
        @operator = operator
        @argument = argument
      end

      def ==(other) = self.class == other.class && operator == other.operator && argument == other.argument

      def children
        [@argument]
      end

      def to_hash
        {class: self.class, operator: @operator, argument: @argument.to_hash}
      end
    end
  end
end
