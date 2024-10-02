module Wid
  module Nodes
    class Nil < Expression
      def ==(other) = self.class == other&.class && value == other&.value

      def children = []
    end
  end
end
