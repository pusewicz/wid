module Wid
  module Nodes
    class String < Expression
      def ==(other) = value == other&.value

      def children = []
    end
  end
end
