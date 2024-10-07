module Wid
  module Nodes
    class Program < Node
      def initialize(body)
        @body = body
      end

      def children
        @body
      end
    end
  end
end
