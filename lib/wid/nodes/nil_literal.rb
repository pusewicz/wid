module Wid
  module Nodes
    class NilLiteral < Node
      def ==(other) = self.class == other&.class

      def children = []

      def to_hash = {class: self.class}
    end
  end
end
