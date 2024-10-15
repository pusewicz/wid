module Wid
  module Nodes
    class ReturnStatement < Node
      attr_reader :argument

      def initialize(argument)
        @argument = argument
      end

      def children
        [@argument]
      end

      def to_hash
        {class: self.class, argument: @argument&.to_hash}
      end
    end
  end
end
