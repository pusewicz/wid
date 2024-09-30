module Wid
  module Nodes
    class String < Expression
      # Use super's initializer

      def ==(other) = value == other&.value
      def children = []
    end
  end
end
