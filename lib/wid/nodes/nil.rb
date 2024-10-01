module Wid
  module Nodes
    class Nil < Expression
      def initialize = super
      def ==(other) = self.class == other&.class && value == other&.value
      def children = []
    end
  end
end
