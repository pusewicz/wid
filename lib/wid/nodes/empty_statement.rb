module Wid
  module Nodes
    class EmptyStatement < Node
      def initialize
      end

      def children = []
    end

    def to_hash
      {class: self.class}
    end
  end
end
