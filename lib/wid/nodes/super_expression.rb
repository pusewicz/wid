module Wid
  module Nodes
    class SuperExpression < Node
      def initialize
      end

      def to_hash
        {class: self.class}
      end
    end
  end
end
