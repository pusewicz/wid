module Wid
  module Nodes
    class Node
      def accept(visitor) = visitor.visit(self)
    end
  end
end
