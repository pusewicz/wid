module Wid
  module Nodes
    class SelfExpression < Node
      def initialize
      end

      def to_hash
        {class: self.class}
      end
    end
  end
end
